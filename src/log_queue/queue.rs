use super::config::LogQueueConfig;
use super::http::save_payload_debug;
use super::prepare::SpanRoute;
use crate::logger::LoginState;
use crate::types::{ParentSpanInfo, SpanPayload};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use url::Url;

/// Supplies the bearer token for queued uploads when they are sent.
///
/// The queue calls [`TokenProvider::token`] once per upload job, on the runtime
/// that submitted the job's rows. Cache tokens that are expensive to mint. Each
/// call is bounded by [`crate::ServiceConfig::token_provider_timeout`], and a
/// failed call is retried like an upload. Once retries run out, the rows
/// waiting on it are dropped and counted in [`DroppedRows::upload_failed`].
#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Return the token for the next upload.
    async fn token(&self) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

/// How a queued item authenticates its upload.
#[derive(Clone)]
pub(crate) enum Credential {
    Token(Arc<str>),
    /// Fetched when the item is sent. Items share a credential only if they share
    /// the provider `Arc`.
    Provider(Arc<dyn TokenProvider>),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Token(_) => "Token([redacted])",
            Self::Provider(_) => "TokenProvider",
        })
    }
}

impl Credential {
    pub(crate) fn token_value(token: String) -> Self {
        Self::Token(Arc::from(token))
    }

    /// Identifies the credential when no organization is known. Providers are
    /// identified by instance, matching how they partition work.
    pub(crate) fn stand_in(&self) -> String {
        match self {
            Self::Token(token) => token.to_string(),
            Self::Provider(provider) => format!("provider:{:p}", Arc::as_ptr(provider)),
        }
    }

    pub(crate) async fn token(&self) -> std::result::Result<Cow<'_, str>, anyhow::Error> {
        match self {
            Self::Token(token) => Ok(Cow::Borrowed(token.as_ref())),
            Self::Provider(provider) => tokio::time::timeout(
                crate::service::global()?.config.token_provider_timeout,
                provider.token(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("token provider timed out"))?
            .map(Cow::Owned)
            .map_err(|error| anyhow::anyhow!("token provider failed: {error}")),
        }
    }
}

impl PartialEq for Credential {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Token(a), Self::Token(b)) => a == b,
            (Self::Provider(a), Self::Provider(b)) => {
                std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
            }
            _ => false,
        }
    }
}

impl Eq for Credential {}

impl Hash for Credential {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Token(token) => token.hash(state),
            Self::Provider(provider) => Arc::as_ptr(provider).cast::<()>().hash(state),
        }
    }
}

pub(crate) struct SubmitCommand {
    pub(crate) credential: Credential,
    pub(super) item: QueuedItem,
}

// Spans are the common case; boxing them would add an allocation per span.
#[allow(clippy::large_enum_variant)]
pub(super) enum QueuedItem {
    /// A span payload, turned into a row at flush time.
    Span {
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    },
    /// A caller-built row, uploaded as given.
    Row {
        org_id: String,
        org_name: Option<String>,
        row: Map<String, Value>,
    },
}

/// Cumulative counts of rows the log queue discarded instead of uploading.
///
/// Counts only grow over the life of the client, so callers can export them as
/// monotonic counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DroppedRows {
    /// Rejected because the queue was full or already closed.
    pub queue_full: u64,
    /// Discarded because the row could not be prepared or serialized for upload.
    pub preparation_failed: u64,
    /// Discarded after an upload failed and any retries were exhausted.
    pub upload_failed: u64,
}

/// Settings shared by a client and its derived handles.
pub(crate) struct QueueSettings {
    pub(crate) config: Arc<LogQueueConfig>,
    pub(crate) client: crate::http::SharedHttp,
    pub(crate) api_url: Url,
    pub(crate) app_url: Url,
}

pub(crate) struct LogQueueCore {
    pub(crate) settings: Arc<QueueSettings>,
    pub(crate) observation: Arc<super::observation::Observation>,
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    queue_depth_warning_emitted: AtomicBool,
    pub(super) login_state: LoginState,
}

impl LogQueueCore {
    fn new(
        settings: Arc<QueueSettings>,
        login_state: LoginState,
        parent: Option<Arc<super::observation::Observation>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            login_state,
            observation: Arc::new(super::observation::Observation::new(parent)),
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            queue_depth_warning_emitted: AtomicBool::new(false),
        })
    }

    pub(crate) fn route(&self, org_id: &str, org_name: Option<&str>) -> (String, Option<String>) {
        let same_org = self.login_state.org_id().as_deref() == Some(org_id)
            || (org_id.is_empty()
                && org_name.is_some()
                && self.login_state.org_name().as_deref() == org_name);
        let endpoint = same_org
            .then(|| self.login_state.api_url())
            .flatten()
            .unwrap_or_else(|| self.settings.api_url.to_string());
        let name = org_name
            .map(str::to_owned)
            .or_else(|| same_org.then(|| self.login_state.org_name()).flatten());
        (endpoint, name)
    }

    fn push(
        self: &Arc<Self>,
        cmd: SubmitCommand,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        crate::service::global()
            .expect("initialized service")
            .engine
            .submit(self.clone(), cmd)
    }

    pub(crate) fn rejected(&self, cmd: SubmitCommand) {
        self.dropped_count.fetch_add(1, Ordering::Relaxed);
        self.observation.queue_full();
        self.log_drop_warning();
        self.dump_dropped_row_if_configured(cmd);
    }

    pub(crate) fn retained(&self, rows: usize) {
        if rows == 1 {
            self.queue_depth_warning_emitted
                .store(false, Ordering::Relaxed);
        }
        self.log_queue_depth_warning(rows);
    }

    /// Log a drop warning with throttling.
    ///
    /// Reports the number of items dropped since the last warning was emitted,
    /// then resets the counter. Throttled to at most once per `queue_drop_logging_period`.
    fn log_drop_warning(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let last_log = self.last_drop_log_time.load(Ordering::Relaxed);
        let throttle_secs = self.settings.config.queue_drop_logging_period().as_secs();

        if now - last_log >= throttle_secs
            && self
                .last_drop_log_time
                .compare_exchange(last_log, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let count = self.dropped_count.swap(0, Ordering::Relaxed);
            if count > 0 {
                warn!(
                    "Log queue full (size: {}), dropped {} items since last warning. \
                     Consider increasing BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE.",
                    self.settings.config.queue_max_size(),
                    count
                );
            }
        }
    }

    /// Warn once per fill cycle when the queue reaches 80% of its configured maximum.
    fn log_queue_depth_warning(&self, queue_size: usize) {
        let queue_max_size = self.settings.config.queue_max_size();
        let warning_threshold = queue_max_size.saturating_mul(4).div_ceil(5);

        if queue_size >= warning_threshold
            && !self
                .queue_depth_warning_emitted
                .swap(true, Ordering::Relaxed)
        {
            warn!(
                queue_size,
                queue_max_size, "Log queue has reached 80% of the configured maximum depth"
            );
        }
    }

    /// If debug directories are configured, serialize the dropped command's payload
    /// and write it to disk. Spawns a background task — best-effort, non-blocking.
    fn dump_dropped_row_if_configured(&self, cmd: SubmitCommand) {
        let all_dir = self.settings.config.all_publish_payloads_dir();
        let failed_dir = self.settings.config.failed_publish_payloads_dir();
        let dirs: Vec<_> = [all_dir, failed_dir].into_iter().flatten().collect();
        if dirs.is_empty() {
            return;
        }
        let service = crate::service::global().expect("initialized service");
        service.spawn_operation(async move {
            let SubmitCommand {
                credential: _,
                item,
            } = cmd;
            let (payload, parent_info) = match item {
                QueuedItem::Span {
                    payload,
                    parent_info,
                } => (payload, parent_info),
                QueuedItem::Row { row, .. } => {
                    write_dropped_row_debug(dirs, &row);
                    return;
                }
            };

            let Ok(route) = SpanRoute::new(&payload, parent_info.as_ref()) else {
                return;
            };
            let Some(row) = route.into_known_row(payload) else {
                return;
            };
            write_dropped_row_debug(dirs, &row);
        });
    }
}

/// Write a dropped row to each debug directory as a one-row logs3 payload.
fn write_dropped_row_debug(dirs: Vec<std::path::PathBuf>, row: &impl serde::Serialize) {
    let payload_bytes = match serde_json::to_vec(&serde_json::json!({
        "rows": [row],
        "api_version": crate::types::LOGS_API_VERSION,
    })) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "failed to serialize dropped row for debug dump");
            return;
        }
    };
    for dir in dirs {
        if let Err(e) = save_payload_debug(&payload_bytes, Some(dir), "dropped") {
            warn!(error = %e, "failed to write dropped row to debug dir");
        }
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum PartitionDestination {
    Span(super::prepare::Destination),
    InvalidSpan,
    Raw(String),
}

impl SubmitCommand {
    #[cfg(test)]
    pub(crate) fn test_row(id: &str) -> Self {
        Self::test_scoped_row(id, "project", "org")
    }

    #[cfg(test)]
    pub(crate) fn test_scoped_row(id: &str, project: &str, org: &str) -> Self {
        Self {
            credential: Credential::token_value("token".into()),
            item: QueuedItem::Row {
                org_id: org.into(),
                org_name: None,
                row: serde_json::json!({"id":id,"project_id":project})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        }
    }
    #[cfg(test)]
    pub(crate) fn test_span(is_merge: bool) -> Self {
        Self {
            credential: Credential::token_value("token".into()),
            item: QueuedItem::Span {
                payload: SpanPayload {
                    row_id: "same-row".into(),
                    span_id: "span".into(),
                    is_merge,
                    span_components: None,
                    org_id: "org".into(),
                    org_name: None,
                    project_name: None,
                    input: None,
                    output: Some(serde_json::json!({"merge":is_merge})),
                    expected: None,
                    error: None,
                    scores: None,
                    metadata: None,
                    metrics: None,
                    tags: None,
                    context: None,
                    span_attributes: None,
                    extra: Default::default(),
                },
                parent_info: Some(ParentSpanInfo::ProjectLogs {
                    object_id: "project".into(),
                }),
            },
        }
    }
    #[cfg(test)]
    pub(crate) fn test_named_span(row_id: &str, project_name: &str) -> Self {
        let mut command = Self::test_span(false);
        if let QueuedItem::Span {
            payload,
            parent_info,
        } = &mut command.item
        {
            payload.row_id = row_id.into();
            payload.span_id = row_id.into();
            payload.project_name = Some(project_name.into());
            *parent_info = None;
        }
        command
    }
    /// The credential and org a command is delivered for.
    pub(crate) fn partition(&self) -> (Credential, String, Option<String>) {
        match &self.item {
            QueuedItem::Span { payload, .. } => (
                self.credential.clone(),
                payload.org_id.clone(),
                payload.org_name.clone(),
            ),
            QueuedItem::Row {
                org_id, org_name, ..
            } => (self.credential.clone(), org_id.clone(), org_name.clone()),
        }
    }
    pub(super) fn destination(&self) -> PartitionDestination {
        match &self.item {
            QueuedItem::Span {
                payload,
                parent_info,
            } => SpanRoute::new(payload, parent_info.as_ref())
                .map_or(PartitionDestination::InvalidSpan, |route| {
                    PartitionDestination::Span(route.destination)
                }),
            QueuedItem::Row { row, .. } => {
                PartitionDestination::Raw(crate::internal::ingest::raw_destination_key(row))
            }
        }
    }
}

/// A scoped facade over the process-wide delivery service.
#[derive(Clone)]
pub struct LogQueue {
    pub(crate) core: Arc<LogQueueCore>,
}

impl LogQueue {
    pub fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: crate::http::SharedHttp,
        _worker_queue_size: usize,
        api_url: Url,
        app_url: Url,
    ) -> Self {
        Self {
            core: LogQueueCore::new(
                Arc::new(QueueSettings {
                    config: Arc::new(config),
                    client,
                    api_url,
                    app_url,
                }),
                login_state,
                None,
            ),
        }
    }

    pub(crate) fn child(&self) -> Self {
        self.child_with_login(self.core.login_state.clone())
    }

    pub(crate) fn child_with_login(&self, state: LoginState) -> Self {
        Self {
            core: LogQueueCore::new(
                self.core.settings.clone(),
                state,
                Some(self.core.observation.clone()),
            ),
        }
    }

    pub(crate) fn rebind(&self, state: LoginState) -> Self {
        Self {
            core: LogQueueCore::new(self.core.settings.clone(), state, None),
        }
    }

    pub(crate) fn submit(
        &self,
        credential: Credential,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        self.core.push(SubmitCommand {
            credential,
            item: QueuedItem::Span {
                payload,
                parent_info,
            },
        })
    }

    pub(crate) fn submit_rows(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        self.submit_rows_with(
            Credential::token_value(token.into()),
            org_id,
            org_name,
            rows,
        );
    }
    pub(crate) fn submit_rows_with_provider(
        &self,
        provider: Arc<dyn TokenProvider>,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        self.submit_rows_with(Credential::Provider(provider), org_id, org_name, rows);
    }
    fn submit_rows_with(
        &self,
        credential: Credential,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        // Never run caller iterator code while holding the shared intake lock.
        let mut rows = rows.into_iter();
        loop {
            let commands: Vec<_> = rows
                .by_ref()
                .take(1024)
                .map(|row| SubmitCommand {
                    credential: credential.clone(),
                    item: QueuedItem::Row {
                        org_id: org_id.into(),
                        org_name: org_name.map(str::to_owned),
                        row,
                    },
                })
                .collect();
            if commands.is_empty() {
                break;
            }
            crate::service::global()
                .expect("initialized service")
                .engine
                .submit_many(self.core.clone(), commands);
        }
    }
    pub(crate) fn dropped_rows(&self) -> DroppedRows {
        self.core.observation.dropped_rows()
    }
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        crate::service::global()
            .expect("initialized service")
            .engine
            .barrier(&self.core.observation)
    }
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> crate::Completion<crate::ShutdownReport, crate::ShutdownError> {
        crate::service::global()
            .expect("initialized service")
            .engine
            .shutdown(&self.core.observation, timeout)
    }
}
