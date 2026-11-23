use super::config::LogQueueConfig;
use super::http::save_payload_debug;
use crate::error::Result;
use crate::logger::LoginState;
use crate::span_components::ProjectLogsIdentifier;
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};
use chrono::Utc;
use indexmap::IndexMap;
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
/// The queue calls [`TokenProvider::token`] when preparing and sending upload
/// groups. Cache tokens that are expensive to mint. Calls run on the shared SDK
/// runtime and are bounded by [`crate::ServiceConfig::preparation_timeout`]. When it
/// returns an error, the rows waiting on it are dropped and counted in
/// [`DroppedRows::upload_failed`].
pub trait TokenProvider: Send + Sync {
    /// Return the token for the next upload.
    fn token(&self) -> crate::Completion<String, Box<dyn std::error::Error + Send + Sync>>;
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
        // Intern only live credentials. Keys are reclaimed and the registry is bounded.
        static TOKENS: std::sync::OnceLock<
            std::sync::Mutex<IndexMap<String, std::sync::Weak<str>>>,
        > = std::sync::OnceLock::new();
        let mut tokens = TOKENS.get_or_init(Default::default).lock().unwrap();
        if let Some(existing) = tokens.get(&token).and_then(std::sync::Weak::upgrade) {
            return Self::Token(existing);
        }
        tokens.retain(|_, value| value.strong_count() > 0);
        if tokens.len() >= 256 {
            tokens.shift_remove_index(0);
        }
        let value: Arc<str> = Arc::from(token.as_str());
        tokens.insert(token, Arc::downgrade(&value));
        Self::Token(value)
    }

    pub(crate) async fn token(&self) -> std::result::Result<Cow<'_, str>, anyhow::Error> {
        match self {
            Self::Token(token) => Ok(Cow::Borrowed(token.as_ref())),
            Self::Provider(provider) => tokio::time::timeout(
                crate::service::global()?.config.preparation_timeout,
                provider.token().cancel_on_drop(),
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

/// Immutable routing and policy for a facade. Queue storage and workers live in Service.
pub(crate) struct LogQueueCore {
    pub(crate) config: LogQueueConfig,
    pub(crate) observation: Arc<super::observation::Observation>,
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    queue_depth_warning_emitted: AtomicBool,
    pub(super) login_state: LoginState,
    pub(super) client: crate::http::SharedHttp,
    pub(super) api_url: Url,
    pub(super) app_url: Url,
    pub(super) routing_org_name: Option<String>,
    routing_api_url: Option<String>,
}

impl LogQueueCore {
    fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: crate::http::SharedHttp,
        api_url: Url,
        app_url: Url,
        parent: Option<Arc<super::observation::Observation>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            login_state,
            client,
            api_url,
            app_url,
            routing_org_name: None,
            routing_api_url: None,
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
        let endpoint = if same_org {
            self.endpoint()
        } else {
            self.api_url.to_string()
        };
        let name = org_name
            .map(str::to_owned)
            .or_else(|| same_org.then(|| self.login_state.org_name()).flatten());
        (endpoint, name)
    }

    pub(crate) fn job(&self, endpoint: &str, org_name: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            config: self.config.clone(),
            observation: self.observation.clone(),
            login_state: LoginState::new(),
            client: self.client.clone(),
            api_url: self.api_url.clone(),
            routing_api_url: Some(endpoint.to_owned()),
            app_url: self.app_url.clone(),
            routing_org_name: org_name.map(str::to_owned),
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            queue_depth_warning_emitted: AtomicBool::new(false),
        })
    }

    fn push(
        self: &Arc<Self>,
        cmd: SubmitCommand,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        crate::service::global()
            .expect("initialized service")
            .submit(self.clone(), cmd)
    }

    pub(crate) fn rejected(&self, cmd: SubmitCommand) {
        self.dropped_count.fetch_add(1, Ordering::Relaxed);
        self.observation.queue_full();
        self.log_drop_warning();
        self.dump_dropped_row_if_configured(cmd);
    }

    pub(crate) fn endpoint(&self) -> String {
        self.routing_api_url
            .clone()
            .or_else(|| self.login_state.api_url())
            .unwrap_or_else(|| self.api_url.to_string())
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
        let throttle_secs = self.config.queue_drop_logging_period().as_secs();

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
                    self.config.queue_max_size(),
                    count
                );
            }
        }
    }

    /// Warn once per fill cycle when the queue reaches 80% of its configured maximum.
    fn log_queue_depth_warning(&self, queue_size: usize) {
        let queue_max_size = self.config.queue_max_size();
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
        let all_dir = self.config.all_publish_payloads_dir();
        let failed_dir = self.config.failed_publish_payloads_dir();
        let dirs: Vec<_> = [all_dir, failed_dir].into_iter().flatten().collect();
        if dirs.is_empty() {
            return;
        }
        let handle = crate::service::global()
            .expect("initialized service")
            .handle
            .clone();
        handle.spawn(async move {
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

            let span_components = payload.span_components.clone();

            let span_id = payload.span_id.clone();
            let destination = match span_components.as_ref() {
                Some(span_components) => match (
                    span_components.object_type,
                    span_components.object_id.as_ref(),
                ) {
                    (SpanObjectType::Experiment, Some(object_id)) => {
                        LogDestination::experiment(object_id.clone())
                    }
                    (SpanObjectType::ProjectLogs, Some(object_id)) => {
                        LogDestination::project_logs(object_id.clone())
                    }
                    (SpanObjectType::PlaygroundLogs, Some(object_id)) => {
                        LogDestination::playground_logs(object_id.clone())
                    }
                    (SpanObjectType::ProjectLogs, None) => {
                        match span_components.project_logs_identifier() {
                            Some(ProjectLogsIdentifier::ObjectId(project_id)) => {
                                LogDestination::project_logs(project_id)
                            }
                            _ => return,
                        }
                    }
                    (SpanObjectType::Experiment, None) => return,
                    (SpanObjectType::PlaygroundLogs, None) => return,
                },
                None => match parent_info.as_ref() {
                    Some(ParentSpanInfo::Experiment { object_id }) => {
                        LogDestination::experiment(object_id.clone())
                    }
                    Some(ParentSpanInfo::ProjectLogs { object_id }) => {
                        LogDestination::project_logs(object_id.clone())
                    }
                    Some(ParentSpanInfo::ProjectName { .. }) => return,
                    Some(ParentSpanInfo::PlaygroundLogs { object_id }) => {
                        LogDestination::playground_logs(object_id.clone())
                    }
                    Some(ParentSpanInfo::Dataset { object_id }) => {
                        LogDestination::dataset(object_id.clone())
                    }
                    Some(ParentSpanInfo::FullSpan {
                        object_type,
                        object_id,
                        ..
                    }) => match object_type {
                        SpanObjectType::Experiment => match object_id.clone() {
                            Some(object_id) => LogDestination::experiment(object_id),
                            None => return,
                        },
                        SpanObjectType::ProjectLogs => match object_id.clone() {
                            Some(object_id) => LogDestination::project_logs(object_id),
                            None => return,
                        },
                        SpanObjectType::PlaygroundLogs => match object_id.clone() {
                            Some(object_id) => LogDestination::playground_logs(object_id),
                            None => return,
                        },
                    },
                    None => return,
                },
            };

            let root_span_id = span_components
                .as_ref()
                .and_then(|components| components.root_span_id.clone())
                .unwrap_or_else(|| span_id.clone());
            let row = Logs3Row {
                id: payload.row_id,
                span_id: span_id.clone(),
                is_merge: if payload.is_merge { Some(true) } else { None },
                merge_paths: None,
                root_span_id,
                span_parents: span_components
                    .as_ref()
                    .and_then(|components| components.span_parents.clone()),
                destination,
                org_id: payload.org_id,
                org_name: payload.org_name,
                input: payload.input,
                output: payload.output,
                expected: payload.expected,
                error: payload.error,
                scores: payload.scores,
                metadata: payload.metadata,
                metrics: payload.metrics,
                tags: payload.tags,
                context: payload.context,
                span_attributes: payload.span_attributes,
                extra: payload.extra,
                created: Utc::now(),
                xact_id: None,
                object_delete: None,
                audit_source: Some("api".to_string()),
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
    pub(crate) fn partition(
        &self,
    ) -> (
        Credential,
        String,
        Option<String>,
        crate::destination::Destination,
    ) {
        match &self.item {
            QueuedItem::Span {
                payload,
                parent_info,
            } => (
                self.credential.clone(),
                payload.org_id.clone(),
                payload.org_name.clone(),
                crate::destination::Destination::span(payload, parent_info.as_ref()),
            ),
            QueuedItem::Row {
                org_id,
                org_name,
                row,
            } => (
                self.credential.clone(),
                org_id.clone(),
                org_name.clone(),
                crate::destination::Destination::row(row),
            ),
        }
    }
    pub(crate) fn is_raw(&self) -> bool {
        matches!(self.item, QueuedItem::Row { .. })
    }
    pub(crate) fn estimated_bytes(&self) -> usize {
        // The serializer counts bytes without allocating a second encoded payload.
        struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(std::mem::size_of::<Self>());
        match &self.item {
            QueuedItem::Row { row, .. } => {
                let _ = serde_json::to_writer(&mut count, row);
            }
            QueuedItem::Span { payload, .. } => {
                let _ = serde_json::to_writer(&mut count, payload);
            }
        }
        count.0
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
            core: LogQueueCore::new(config, login_state, client, api_url, app_url, None),
        }
    }

    pub(crate) fn child(&self) -> Self {
        self.child_with_login(self.core.login_state.clone())
    }

    pub(crate) fn child_with_login(&self, state: LoginState) -> Self {
        Self {
            core: LogQueueCore::new(
                self.core.config.clone(),
                state,
                self.core.client.clone(),
                self.core.api_url.clone(),
                self.core.app_url.clone(),
                Some(self.core.observation.clone()),
            ),
        }
    }

    pub(crate) fn rebind(&self, state: LoginState) -> Self {
        Self {
            core: LogQueueCore::new(
                self.core.config.clone(),
                state,
                self.core.client.clone(),
                self.core.api_url.clone(),
                self.core.app_url.clone(),
                None,
            ),
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
                .submit_many(self.core.clone(), commands);
        }
    }
    pub(crate) fn dropped_rows(&self) -> DroppedRows {
        self.core.observation.dropped_rows()
    }
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        crate::service::global()
            .expect("initialized service")
            .barrier(&self.core.observation, true)
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
    pub async fn trigger_flush_command(&self) -> Result<()> {
        crate::service::global()?.activate(&self.core.observation);
        Ok(())
    }
}

impl Drop for LogQueue {
    fn drop(&mut self) {
        // Never block a destructor or shut down machinery shared by other clients.
        if let Ok(service) = crate::service::global() {
            service.activate(&self.core.observation);
        }
    }
}
