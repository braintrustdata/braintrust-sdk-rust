use super::batching::{batch_and_serialize_maps, batch_and_serialize_rows, SerializedBatch};
use super::config::LogQueueConfig;
use super::http::{fetch_version_info, save_payload_debug, send_batch_counting};
use super::merge::merge_row_into;
use super::prepare::SpanRoute;
use super::row_key::RowKey;
use crate::error::Result;
use crate::internal::delivery::WriteService;
use crate::logger::LoginState;
use crate::types::{Logs3Row, ParentSpanInfo, SpanPayload};
use crate::{
    Completion, DeliveryReceipt, FlushError, FlushReport, ShutdownError, ShutdownReport, WriteError,
};
use futures::stream::{self, StreamExt};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use url::Url;

/// Supplies the bearer token for queued uploads when they are sent.
///
/// The queue calls [`TokenProvider::token`] once per upload group each time it
/// flushes. Resolving an uncached project name can also request a token.
/// Cache tokens that are expensive to mint, and bound how long a call
/// can take: a call that never returns stalls this client's uploads. When it
/// returns an error, the rows waiting on it are dropped and counted in
/// [`DroppedRows::upload_failed`].
#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Return the token for the next upload.
    async fn token(&self) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

/// How a queued item authenticates its upload.
#[derive(Clone)]
pub(crate) enum Credential {
    Token(String),
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
    async fn token(&self) -> std::result::Result<Cow<'_, str>, anyhow::Error> {
        match self {
            Self::Token(token) => Ok(Cow::Borrowed(token)),
            Self::Provider(provider) => provider
                .token()
                .await
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

/// An item queued for deferred preparation and flushing, with the credential used
/// to upload it.
///
/// Fields are private — construction and destructuring happen only within this
/// module. `worker.rs` never touches the fields directly.
struct SubmitCommand {
    credential: Credential,
    item: QueuedItem,
}

// Spans are the common case; boxing them would add an allocation per span.
#[allow(clippy::large_enum_variant)]
enum QueuedItem {
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

#[derive(Default)]
struct DropCounters {
    queue_full: AtomicU64,
    preparation_failed: AtomicU64,
    upload_failed: AtomicU64,
}

impl DropCounters {
    fn snapshot(&self) -> DroppedRows {
        DroppedRows {
            queue_full: self.queue_full.load(Ordering::Relaxed),
            preparation_failed: self.preparation_failed.load(Ordering::Relaxed),
            upload_failed: self.upload_failed.load(Ordering::Relaxed),
        }
    }
}

/// Rows bound for one upload credential (token or token provider, and org).
struct CredentialGroup {
    org_name: Option<String>,
    spans: Vec<Logs3Row>,
    rows: Vec<Map<String, Value>>,
}

/// Preparation and protocol orchestration, independent of admission/lifecycle.
struct SpanBatchProcessor {
    drops: Arc<DropCounters>,
    config: LogQueueConfig,
    login_state: LoginState,
    client: crate::http::SharedHttp,
    api_url: Url,
    app_url: Url,
}
impl SpanBatchProcessor {
    fn new(
        config: LogQueueConfig,
        drops: Arc<DropCounters>,
        login_state: LoginState,
        client: crate::http::SharedHttp,
        api_url: Url,
        app_url: Url,
    ) -> Arc<Self> {
        Arc::new(Self {
            drops,
            config,
            login_state,
            client,
            api_url,
            app_url,
        })
    }

    /// Prepare, merge, batch and upload one detached delivery generation.
    ///
    /// Commands are drained atomically. Each command is prepared (project
    /// registration, row building) before batching. Each credential group's spans
    /// are merged by row key, then its spans and caller-built rows are batched and sent with at most
    /// `max_concurrent_log_requests` requests in flight (matching the TypeScript SDK).
    async fn process(
        self: &Arc<Self>,
        cmds: Vec<SubmitCommand>,
    ) -> std::result::Result<(), anyhow::Error> {
        if cmds.is_empty() {
            return Ok(());
        }
        let mut first_error = None;

        // Resolve the data-plane URL. Prefer the shared login state (covers the
        // logged-in path, including an org-specific api_url returned by login) and
        // fall back to the client-configured URL so per-request-credential callers
        // work without ever populating the shared login state.
        let api_url_str = self
            .login_state
            .api_url()
            .unwrap_or_else(|| self.api_url.to_string());
        let api_url = match Url::parse(&api_url_str) {
            Ok(url) => url,
            Err(e) => {
                warn!(error = %e, "Invalid API URL");
                self.drops
                    .upload_failed
                    .fetch_add(cmds.len() as u64, Ordering::Relaxed);
                return Err(e.into());
            }
        };

        // Prepare rows and bucket them by their upload credentials. The batch
        // "destination" is (credential, org_id): rows uploaded with different
        // credentials or orgs must never be merged or batched into the same request
        // under one token. `org_name` is carried alongside for the `x-bt-org-name`
        // header.
        let mut groups: IndexMap<(Credential, String), CredentialGroup> = IndexMap::new();
        for cmd in cmds {
            let SubmitCommand { credential, item } = cmd;
            match item {
                QueuedItem::Span {
                    payload,
                    parent_info,
                } => match self.prepare_row(&credential, payload, parent_info).await {
                    Ok(row) => {
                        let org_id = row.org_id.clone();
                        let org_name = row.org_name.clone();
                        self.credential_group(&mut groups, credential, org_id, org_name)
                            .spans
                            .push(row);
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to prepare span, dropping");
                        self.drops
                            .preparation_failed
                            .fetch_add(1, Ordering::Relaxed);
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                    }
                },
                QueuedItem::Row {
                    org_id,
                    org_name,
                    row,
                } => self
                    .credential_group(&mut groups, credential, org_id, org_name)
                    .rows
                    .push(row),
            }
        }

        if groups.is_empty() {
            return first_error.map_or(Ok(()), Err);
        }

        for ((credential, _org_id), group) in groups {
            let CredentialGroup {
                org_name,
                spans,
                rows,
            } = group;
            // Tokens from a provider are fetched now, so rows that waited in the
            // queue still go out with a current token.
            let token = match credential.token().await {
                Ok(token) => token,
                Err(e) => {
                    warn!(error = %e, "failed to get upload token, dropping rows");
                    self.drops
                        .upload_failed
                        .fetch_add((spans.len() + rows.len()) as u64, Ordering::Relaxed);
                    first_error.get_or_insert(e);
                    continue;
                }
            };

            // Version info reflects a server-wide limit, so it is fetched once
            // (with the first group's credentials) and cached.
            let (max_request_size, can_use_overflow) =
                fetch_version_info(&self.client.client(), &api_url, &token, org_name.as_deref())
                    .await;

            // Effective batch byte limit: the smaller of the locally configured limit and half the
            // server-advertised max request size. This mirrors the TypeScript SDK which uses
            // `maxRequestSize / 2` as the batching limit, where maxRequestSize comes from /version.
            let effective_batch_bytes = self.config.batch_max_bytes().min(max_request_size / 2);

            if !spans.is_empty() {
                let config = self.config.clone();
                let count = spans.len();
                let batches = tokio::task::spawn_blocking(move || {
                    let mut merged: IndexMap<RowKey, Logs3Row> = IndexMap::new();
                    for row in spans {
                        let key = RowKey::from_row(&row);
                        if let Some(existing) = merged.get_mut(&key) {
                            if row.is_merge.unwrap_or(false) {
                                merge_row_into(existing, row);
                            } else {
                                *existing = row;
                            }
                        } else {
                            merged.insert(key, row);
                        }
                    }

                    let rows: Vec<Logs3Row> = merged.into_values().collect();
                    batch_and_serialize_rows(rows, &config, effective_batch_bytes)
                })
                .await;
                let batches = self.serialized_batches(batches, count, &mut first_error);
                if let Err(e) = self
                    .send_batches(
                        &api_url,
                        &token,
                        org_name.as_deref(),
                        batches,
                        max_request_size,
                        can_use_overflow,
                    )
                    .await
                {
                    first_error.get_or_insert(e);
                }
            }

            // Caller-built rows are sent as given: no merging by row key.
            if !rows.is_empty() {
                let config = self.config.clone();
                let count = rows.len();
                let batches = tokio::task::spawn_blocking(move || {
                    batch_and_serialize_maps(rows, &config, effective_batch_bytes)
                })
                .await;
                let batches = self.serialized_batches(batches, count, &mut first_error);
                if let Err(e) = self
                    .send_batches(
                        &api_url,
                        &token,
                        org_name.as_deref(),
                        batches,
                        max_request_size,
                        can_use_overflow,
                    )
                    .await
                {
                    first_error.get_or_insert(e);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The upload group for a credential, created on first use. Rows without an
    /// org name fall back to the shared login state's.
    fn credential_group<'a>(
        &self,
        groups: &'a mut IndexMap<(Credential, String), CredentialGroup>,
        credential: Credential,
        org_id: String,
        org_name: Option<String>,
    ) -> &'a mut CredentialGroup {
        groups
            .entry((credential, org_id))
            .or_insert_with(|| CredentialGroup {
                org_name: org_name.or_else(|| self.login_state.org_name()),
                spans: Vec::new(),
                rows: Vec::new(),
            })
    }

    /// Unwrap a chunk's serialization result, counting its rows as dropped on failure.
    fn serialized_batches(
        &self,
        result: std::result::Result<
            std::result::Result<Vec<SerializedBatch>, anyhow::Error>,
            tokio::task::JoinError,
        >,
        count: usize,
        first_error: &mut Option<anyhow::Error>,
    ) -> Vec<SerializedBatch> {
        let error = match result {
            Ok(Ok(batches)) => return batches,
            Ok(Err(error)) => {
                warn!(%error, "failed to serialize batch");
                error
            }
            Err(error) => {
                warn!(%error, "serialization task panicked");
                error.into()
            }
        };
        self.drops
            .preparation_failed
            .fetch_add(count as u64, Ordering::Relaxed);
        first_error.get_or_insert(error);
        Vec::new()
    }

    /// Send batches with at most `max_concurrent_log_requests` in flight (matching the
    /// TypeScript SDK), counting undelivered rows of failed batches as dropped. Returns
    /// the first error.
    #[allow(clippy::too_many_arguments)]
    async fn send_batches(
        &self,
        api_url: &Url,
        token: &str,
        org_name: Option<&str>,
        batches: Vec<SerializedBatch>,
        max_request_size: usize,
        can_use_overflow: bool,
    ) -> std::result::Result<(), anyhow::Error> {
        let send_futures = batches.into_iter().map(|batch| async move {
            let count = batch.len();
            let (delivered, result) = send_batch_counting(
                &self.client.client(),
                api_url,
                token,
                org_name,
                batch,
                &self.config,
                max_request_size,
                can_use_overflow,
            )
            .await;
            if result.is_err() {
                self.drops
                    .upload_failed
                    .fetch_add(count.saturating_sub(delivered) as u64, Ordering::Relaxed);
            }
            result
        });
        let mut first_error = None;
        let results: Vec<_> = stream::iter(send_futures)
            .buffer_unordered(self.config.max_concurrent_log_requests())
            .collect()
            .await;
        for result in results {
            if let Err(e) = result {
                warn!(error = %e, "batch send failed");
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Prepare a `Logs3Row` from a submit command's token, payload, and parent info.
    ///
    /// Resolves the destination (including calling the project registration API
    /// if needed) and builds the complete row for the flush pipeline.
    async fn prepare_row(
        &self,
        credential: &Credential,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> std::result::Result<Logs3Row, anyhow::Error> {
        SpanRoute::new(&payload, parent_info.as_ref())?
            .into_row(payload, async |org_id, org_name, name| {
                self.ensure_project_id(credential, org_id, org_name, name)
                    .await
            })
            .await
    }

    /// Ensure a project ID is available for the given project name, registering
    /// it via the API if it is not yet cached.
    ///
    /// Uses `login_state.app_url()` for the registration endpoint so the URL
    /// doesn't need to be passed separately.
    async fn ensure_project_id(
        &self,
        credential: &Credential,
        org_id: &str,
        org_name: Option<&str>,
        project_name: &str,
    ) -> std::result::Result<String, anyhow::Error> {
        // Prefer the login-state control-plane URL (logged-in path), falling back
        // to the client-configured URL for per-request-credential callers.
        let app_url = match self.login_state.app_url() {
            Some(app_url_str) => {
                Url::parse(&app_url_str).map_err(|e| anyhow::anyhow!("invalid app URL: {e}"))?
            }
            None => self.app_url.clone(),
        };
        // Resolve credentials before keying requests that have no known org.
        let token = if org_id.is_empty() && org_name.is_none_or(str::is_empty) {
            Some(credential.token().await?)
        } else {
            None
        };
        let org = crate::resolution::OrgIdentity::new(
            org_id,
            org_name,
            token.as_deref().unwrap_or_default(),
        );
        let project = crate::resolution::project(app_url.as_str(), org, project_name);
        project
            .get_or_try_init(|| async {
                let token = match token {
                    Some(token) => token,
                    None => credential.token().await?,
                };
                let mut request = crate::api::ProjectRegisterRequest::new(project_name.to_owned());
                request.org_id = (!org_id.is_empty()).then(|| org_id.to_owned());
                request.org_name = org_name.map(str::to_owned);
                let client = crate::http::api_client(
                    &self.client.client(),
                    self.api_url.as_str(),
                    app_url.as_str(),
                    &token,
                    org_name,
                )?;
                let response = crate::http::api_result(client.register_project(&request).await)
                    .await?
                    .into_inner();
                Ok(response.project.id)
            })
            .await
            .cloned()
    }
}

/// Reports rejected submissions without coupling admission to batch processing.
struct RejectionReporter {
    drops: Arc<DropCounters>,
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    config: LogQueueConfig,
}

impl RejectionReporter {
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

    /// If debug directories are configured, serialize the dropped command's payload
    /// and write it to disk. Spawns a background task — best-effort, non-blocking.
    fn dump_dropped_row_if_configured(&self, cmd: SubmitCommand) {
        let all_dir = self.config.all_publish_payloads_dir();
        let failed_dir = self.config.failed_publish_payloads_dir();
        let dirs: Vec<_> = [all_dir, failed_dir].into_iter().flatten().collect();
        if dirs.is_empty() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
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

/// Compatibility adapter connecting span preparation to the bounded write service.
#[derive(Clone)]
pub struct LogQueue {
    rejections: Arc<RejectionReporter>,
    writes: WriteService<SubmitCommand>,
}
impl LogQueue {
    pub fn new(
        runtime: tokio::runtime::Handle,
        config: LogQueueConfig,
        login_state: LoginState,
        client: crate::http::SharedHttp,
        _worker_queue_size: usize,
        api_url: Url,
        app_url: Url,
    ) -> Self {
        let capacity = config.queue_max_size();
        let drops = Arc::new(DropCounters::default());
        let rejections = Arc::new(RejectionReporter {
            drops: drops.clone(),
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            config: config.clone(),
        });
        let processor =
            SpanBatchProcessor::new(config, drops, login_state, client, api_url, app_url);
        let writes = WriteService::new(capacity, runtime, move |commands| {
            let processor = processor.clone();
            Box::pin(async move { processor.process(commands).await })
        });
        Self { rejections, writes }
    }

    pub(crate) fn submit(
        &self,
        credential: Credential,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Completion<DeliveryReceipt, WriteError> {
        self.enqueue(SubmitCommand {
            credential,
            item: QueuedItem::Span {
                payload,
                parent_info,
            },
        })
    }

    fn enqueue(&self, command: SubmitCommand) -> Completion<DeliveryReceipt, WriteError> {
        self.writes
            .enqueue_with_rejection(command, |command, error| {
                self.rejections
                    .drops
                    .queue_full
                    .fetch_add(1, Ordering::Relaxed);
                self.rejections
                    .dropped_count
                    .fetch_add(1, Ordering::Relaxed);
                if matches!(error, crate::EnqueueError::Full) {
                    self.rejections.log_drop_warning();
                } else {
                    warn!(%error, "span submission rejected");
                }
                self.rejections.dump_dropped_row_if_configured(command);
            })
    }

    /// Submit caller-built rows, uploaded as given with `token` and `org_id`.
    ///
    /// Each row takes one queue slot, so a full queue drops the rows that don't fit.
    pub(crate) fn submit_rows(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        self.submit_rows_with(Credential::Token(token.to_owned()), org_id, org_name, rows);
    }

    /// Submit caller-built rows whose token is fetched from `provider` when they
    /// are sent.
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
        for row in rows {
            self.enqueue(SubmitCommand {
                credential: credential.clone(),
                item: QueuedItem::Row {
                    org_id: org_id.to_owned(),
                    org_name: org_name.map(str::to_owned),
                    row,
                },
            });
        }
    }

    /// Cumulative counts of discarded rows.
    pub(crate) fn dropped_rows(&self) -> DroppedRows {
        self.rejections.drops.snapshot()
    }

    pub fn flush(&self) -> Completion<FlushReport, FlushError> {
        self.writes.flush()
    }
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> Completion<ShutdownReport, ShutdownError> {
        self.writes.shutdown(timeout)
    }
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }
    pub async fn trigger_flush_command(&self) -> Result<()> {
        let token = self.flush();
        if let Err(error) = token.submission() {
            return Err(error.clone().into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn payload(id: &str, merge: bool) -> SpanPayload {
        SpanPayload {
            row_id: id.into(),
            span_id: id.into(),
            is_merge: merge,
            span_components: None,
            org_id: "org".into(),
            org_name: Some("tenant".into()),
            project_name: None,
            input: Some(json!({"value": if merge { 2 } else { 1 }})),
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            extra: std::collections::HashMap::new(),
        }
    }
    fn queue(server: &MockServer) -> LogQueue {
        // Keep the worker on the test thread so submissions made before the
        // first await are drained in one batch.
        LogQueue::new(
            tokio::runtime::Handle::current(),
            LogQueueConfig::builder().num_retries(0).build(),
            LoginState::new(),
            crate::http::build_http_client(std::time::Duration::from_secs(30), None, None).unwrap(),
            32,
            Url::parse(&server.uri()).unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
    }
    fn parent() -> Option<ParentSpanInfo> {
        Some(ParentSpanInfo::Experiment {
            object_id: "experiment".into(),
        })
    }

    #[tokio::test]
    async fn late_route_failure_fails_merged_members_and_successful_siblings_then_retires() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&server)
            .await;
        let queue = queue(&server);
        let first = queue.submit(
            Credential::Token("good".into()),
            payload("merged", false),
            parent(),
        );
        let update = queue.submit(
            Credential::Token("good".into()),
            payload("merged", true),
            parent(),
        );
        let last = queue.submit(
            Credential::Token("bad".into()),
            payload("other", false),
            parent(),
        );
        let Err(FlushError::Delivery(failure)) = queue.flush().await else {
            panic!("expected failed flush");
        };
        assert_eq!(
            failure.failed, 3,
            "count accepted operations before merging"
        );
        for token in [first, update, last] {
            let Err(WriteError::Delivery(error)) = token.await else {
                panic!("expected batch error");
            };
            assert!(Arc::ptr_eq(&error, &failure.cause));
        }
        assert!(queue.is_empty());
        assert_eq!(queue.flush().await.unwrap().delivered, 0);
        let requests = server.received_requests().await.unwrap();
        let uploads: Vec<_> = requests
            .iter()
            .filter(|req| req.url.path() == "/logs3")
            .collect();
        assert_eq!(uploads.len(), 2);
        let first_body: Value = serde_json::from_slice(&uploads[0].body).unwrap();
        assert_eq!(first_body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(first_body["rows"][0]["input"]["value"], 2);
        let fresh = queue.submit(
            Credential::Token("good".into()),
            payload("fresh", false),
            parent(),
        );
        assert_eq!(queue.flush().await.unwrap().delivered, 1);
        fresh.await.unwrap();
        queue
            .shutdown(std::time::Duration::from_secs(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_split_sibling_fails_every_member_despite_an_earlier_committed_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(|request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let rows = body["rows"].as_array().unwrap();
                ResponseTemplate::new(if rows.len() > 1 {
                    413
                } else if rows[0]["id"] == "late-failure" {
                    400
                } else {
                    200
                })
            })
            .expect(3)
            .mount(&server)
            .await;
        let queue = queue(&server);
        let first = queue.submit(
            Credential::Token("token".into()),
            payload("committed", false),
            parent(),
        );
        let last = queue.submit(
            Credential::Token("token".into()),
            payload("late-failure", false),
            parent(),
        );
        let Err(FlushError::Delivery(failure)) = queue.flush().await else {
            panic!("expected failure");
        };
        assert_eq!(failure.failed, 2);
        for token in [first, last] {
            assert!(matches!(token.await, Err(WriteError::Delivery(_))));
        }
        assert!(queue.is_empty());
        queue.flush().await.unwrap();
        queue
            .shutdown(std::time::Duration::from_secs(1))
            .await
            .unwrap();
    }
}
