use std::sync::Arc;
use std::time::Duration;

use indexmap::map::Entry;
use indexmap::IndexMap;

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Map;
use tokio::sync::{mpsc, oneshot, Notify, RwLock};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

use crate::error::{BraintrustError, Result};
use crate::json_merge::deep_merge;
use crate::span::SpanSubmitter;
use crate::types::{
    LogDestination, Logs3Request, Logs3Row, ParentSpanInfo, SpanAttributes, SpanObjectType,
    SpanPayload, LOGS_API_VERSION,
};

const DEFAULT_QUEUE_SIZE: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_BATCH_MAX_ITEMS: usize = 100;
const DEFAULT_BATCH_MAX_BYTES: usize = 6 * 1024 * 1024; // 6 MB
const DEFAULT_MAX_RETRIES: usize = 3;
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Organization info returned from login.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub api_url: Option<String>,
}

/// Composite key for row merging (mirrors TS generateMergedRowKey).
/// Rows with the same key are merged when `_is_merge: true`.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct RowKey {
    org_id: String,
    project_id: Option<String>,
    experiment_id: Option<String>,
    dataset_id: Option<String>,
    prompt_session_id: Option<String>,
    log_id: Option<String>,
    row_id: String,
}

impl RowKey {
    fn from_row(row: &Logs3Row) -> Self {
        Self {
            org_id: row.org_id.clone(),
            project_id: row.destination.project_id().map(|s| s.to_string()),
            experiment_id: row.destination.experiment_id().map(|s| s.to_string()),
            // TODO: Populate dataset_id once Dataset destination is added to LogDestination
            dataset_id: None,
            prompt_session_id: row.destination.prompt_session_id().map(|s| s.to_string()),
            log_id: row.destination.log_id().map(|s| s.to_string()),
            row_id: row.id.clone(),
        }
    }
}

/// A pending row awaiting batch submission.
struct PendingRow {
    row: Logs3Row,
}

/// Configuration passed to the background worker.
#[derive(Default)]
struct WorkerConfig {
    flush_interval: Option<Duration>,
    batch_max_items: Option<usize>,
    batch_max_bytes: Option<usize>,
    queue_max_size: Option<usize>,
}

/// Response from the login endpoint.
#[derive(Debug, Deserialize)]
struct LoginResponse {
    org_info: Vec<OrgInfo>,
}

/// Logged-in state containing API key and org info.
#[derive(Debug, Clone)]
pub struct LoginState {
    pub api_key: String,
    pub org_id: String,
    pub org_name: String,
    pub api_url: Option<String>,
}

/// Builder for creating a BraintrustClient with configuration.
///
/// Configuration is loaded from environment variables by default,
/// and can be overridden using builder methods.
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk_rust::BraintrustClient;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Using environment variables (BRAINTRUST_API_KEY, etc.)
/// let client = BraintrustClient::builder().build().await?;
///
/// // With explicit configuration
/// let client = BraintrustClient::builder()
///     .api_key("sk-...")
///     .org_name("my-org")
///     .default_project("my-project")
///     .blocking_login(true)
///     .build()
///     .await?;
/// # Ok(())
/// # }
/// ```
pub struct BraintrustClientBuilder {
    api_key: Option<String>,
    app_url: Option<String>,
    api_url: Option<String>,
    org_name: Option<String>,
    default_project: Option<String>,
    queue_size: usize,
    blocking_login: bool,
    /// Interval between periodic background flushes.
    pub flush_interval: Option<Duration>,
    /// Maximum items per HTTP batch.
    pub batch_max_items: Option<usize>,
    /// Maximum bytes per HTTP batch.
    pub batch_max_bytes: Option<usize>,
    /// Maximum queue capacity (None = unlimited).
    pub queue_max_size: Option<usize>,
}

impl BraintrustClientBuilder {
    /// Create a new builder with defaults from environment variables.
    ///
    /// Supported environment variables:
    /// - `BRAINTRUST_API_KEY`: API key for authentication (required)
    /// - `BRAINTRUST_APP_URL`: Braintrust app URL (default: `https://www.braintrust.dev`; see [`DEFAULT_APP_URL`])
    /// - `BRAINTRUST_API_URL`: API endpoint URL (default: `https://api.braintrust.dev`; see [`DEFAULT_API_URL`])
    /// - `BRAINTRUST_ORG_NAME`: Organization name (default: first org from login)
    /// - `BRAINTRUST_DEFAULT_PROJECT`: Default project name
    pub fn new() -> Self {
        Self {
            api_key: std::env::var("BRAINTRUST_API_KEY").ok(),
            app_url: std::env::var("BRAINTRUST_APP_URL").ok(),
            api_url: std::env::var("BRAINTRUST_API_URL").ok(),
            org_name: std::env::var("BRAINTRUST_ORG_NAME").ok(),
            default_project: std::env::var("BRAINTRUST_DEFAULT_PROJECT").ok(),
            queue_size: DEFAULT_QUEUE_SIZE,
            blocking_login: false,
            flush_interval: None,
            batch_max_items: None,
            batch_max_bytes: None,
            queue_max_size: None,
        }
    }

    /// Set the API key (overrides `BRAINTRUST_API_KEY` env var).
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set the interval between periodic background flushes.
    /// Default: 1 second.
    pub fn with_flush_interval(mut self, interval: Duration) -> Self {
        self.flush_interval = Some(interval);
        self
    }

    /// Set the maximum number of items per HTTP batch.
    /// Default: 100 (or `BRAINTRUST_DEFAULT_BATCH_SIZE` env var).
    pub fn with_batch_max_items(mut self, max_items: usize) -> Self {
        self.batch_max_items = Some(max_items);
        self
    }

    /// Set the maximum bytes per HTTP batch.
    /// Default: 6 MB (or `BRAINTRUST_MAX_REQUEST_SIZE` env var).
    pub fn with_batch_max_bytes(mut self, max_bytes: usize) -> Self {
        self.batch_max_bytes = Some(max_bytes);
        self
    }

    /// Set the maximum queue capacity. When full, oldest events are dropped (FIFO).
    /// Default: unlimited (or `BRAINTRUST_QUEUE_SIZE` env var).
    pub fn with_queue_max_size(mut self, max_size: usize) -> Self {
        self.queue_max_size = Some(max_size);
        self
    }

    /// Set the app URL (overrides `BRAINTRUST_APP_URL` env var).
    pub fn app_url(mut self, url: impl Into<String>) -> Self {
        self.app_url = Some(url.into());
        self
    }

    /// Set the API URL (overrides `BRAINTRUST_API_URL` env var).
    pub fn api_url(mut self, url: impl Into<String>) -> Self {
        self.api_url = Some(url.into());
        self
    }

    /// Set the organization name (overrides `BRAINTRUST_ORG_NAME` env var).
    pub fn org_name(mut self, name: impl Into<String>) -> Self {
        self.org_name = Some(name.into());
        self
    }

    /// Set the default project name (overrides `BRAINTRUST_DEFAULT_PROJECT` env var).
    pub fn default_project(mut self, name: impl Into<String>) -> Self {
        self.default_project = Some(name.into());
        self
    }

    /// Set the internal queue size for buffering log events.
    pub fn queue_size(mut self, size: usize) -> Self {
        self.queue_size = size;
        self
    }

    /// Block until login completes (default: false, login happens in background).
    ///
    /// When `false` (default), login happens asynchronously in a background task.
    /// When `true`, the `build()` method waits for login to complete before returning.
    pub fn blocking_login(mut self, blocking: bool) -> Self {
        self.blocking_login = blocking;
        self
    }

    /// Build the client, performing login.
    ///
    /// If `blocking_login` is true, waits for login to complete.
    /// Otherwise, login happens in the background with retry logic.
    pub async fn build(self) -> Result<BraintrustClient> {
        // Validate required fields
        let api_key = self.api_key.ok_or_else(|| {
            BraintrustError::InvalidConfig(
                "API key required: set BRAINTRUST_API_KEY or call .api_key()".into(),
            )
        })?;

        let app_url_str = self.app_url.unwrap_or_else(|| DEFAULT_APP_URL.into());
        let api_url_str = self.api_url.unwrap_or_else(|| DEFAULT_API_URL.into());

        let app_url = Url::parse(&app_url_str)
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid app_url: {}", e)))?;
        let api_url = Url::parse(&api_url_str)
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid api_url: {}", e)))?;

        let http_client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| BraintrustError::InvalidConfig(e.to_string()))?;

        // Build worker config from client config
        let worker_config = WorkerConfig {
            flush_interval: self.flush_interval,
            batch_max_items: self.batch_max_items,
            batch_max_bytes: self.batch_max_bytes,
            queue_max_size: self.queue_max_size,
        };

        let (sender, receiver) = mpsc::channel(self.queue_size.max(32));
        let worker = tokio::spawn(run_worker(
            api_url.clone(),
            app_url.clone(),
            receiver,
            worker_config,
        ));

        let client = BraintrustClient {
            inner: Arc::new(ClientInner {
                api_url,
                app_url,
                sender,
                worker,
                login_state: RwLock::new(None),
                login_notify: Notify::new(),
                http_client,
                default_project: self.default_project,
            }),
        };

        // Perform login
        if self.blocking_login {
            client
                .perform_login(&api_key, self.org_name.as_deref())
                .await?;
        } else {
            client.start_background_login(api_key, self.org_name);
        }

        Ok(client)
    }
}

impl Default for BraintrustClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct BraintrustClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    #[allow(dead_code)]
    api_url: Url,
    app_url: Url,
    sender: mpsc::Sender<LogCommand>,
    #[allow(dead_code)]
    worker: JoinHandle<()>,
    login_state: RwLock<Option<LoginState>>,
    login_notify: Notify,
    http_client: reqwest::Client,
    default_project: Option<String>,
}

impl std::fmt::Debug for ClientInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientInner")
            .field("api_url", &self.api_url)
            .field("app_url", &self.app_url)
            .field("default_project", &self.default_project)
            .finish_non_exhaustive()
    }
}

impl BraintrustClient {
    /// Create a new builder for configuring the client.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use braintrust_sdk_rust::BraintrustClient;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = BraintrustClient::builder()
    ///     .api_key("sk-...")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> BraintrustClientBuilder {
        BraintrustClientBuilder::new()
    }

    /// Check if the client is logged in.
    pub async fn is_logged_in(&self) -> bool {
        self.inner.login_state.read().await.is_some()
    }

    /// Wait for login to complete (useful if using background login).
    ///
    /// Returns the login state once available, or an error if login times out.
    pub async fn wait_for_login(&self) -> Result<LoginState> {
        self.wait_for_login_state().await
    }

    /// Get the current login state, if logged in.
    pub async fn login_state(&self) -> Option<LoginState> {
        self.inner.login_state.read().await.clone()
    }

    /// Create a span builder using the logged-in state and default project.
    ///
    /// This waits for login to complete if it hasn't already.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use braintrust_sdk_rust::BraintrustClient;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = BraintrustClient::builder()
    ///     .api_key("sk-...")
    ///     .default_project("my-project")
    ///     .build()
    ///     .await?;
    ///
    /// let span = client.span_builder().await?.build();
    /// # Ok(())
    /// # }
    /// ```
    pub async fn span_builder(&self) -> Result<crate::span::SpanBuilder<Self>> {
        let state = self.wait_for_login_state().await?;
        let mut builder =
            crate::span::SpanBuilder::new(Arc::new(self.clone()), &state.api_key, &state.org_id);
        if let Some(ref project) = self.inner.default_project {
            builder = builder.project_name(project);
        }
        Ok(builder)
    }

    /// Create a span builder with explicit token and org_id.
    ///
    /// Use this if you already have the org_id and don't want to use the login state.
    pub fn span_builder_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> crate::span::SpanBuilder<Self> {
        let submitter = Arc::new(self.clone());
        crate::span::SpanBuilder::new(submitter, token, org_id)
    }

    /// Perform login synchronously.
    async fn perform_login(&self, api_key: &str, org_name: Option<&str>) -> Result<()> {
        let login_url = self
            .inner
            .app_url
            .join("api/apikey/login")
            .map_err(|e| BraintrustError::InvalidConfig(e.to_string()))?;

        let response = self
            .inner
            .http_client
            .post(login_url)
            .bearer_auth(api_key)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| BraintrustError::Network(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(BraintrustError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let login_response: LoginResponse =
            response.json().await.map_err(|e| BraintrustError::Api {
                status: 200,
                message: format!("Failed to parse login response: {}", e),
            })?;

        // Find matching org or use first
        let org = if let Some(name) = org_name {
            login_response
                .org_info
                .into_iter()
                .find(|o| o.name == name)
                .ok_or_else(|| {
                    BraintrustError::InvalidConfig(format!("Organization '{}' not found", name))
                })?
        } else {
            login_response.org_info.into_iter().next().ok_or_else(|| {
                BraintrustError::InvalidConfig(
                    "No organizations found for this API key".to_string(),
                )
            })?
        };

        let state = LoginState {
            api_key: api_key.to_string(),
            org_id: org.id,
            org_name: org.name,
            api_url: org.api_url,
        };

        *self.inner.login_state.write().await = Some(state);
        self.inner.login_notify.notify_waiters();
        Ok(())
    }

    /// Start login in a background task with retry logic.
    fn start_background_login(&self, api_key: String, org_name: Option<String>) {
        let client = self.clone();
        tokio::spawn(async move {
            // Retry with exponential backoff
            let mut delay = Duration::from_millis(100);
            let max_delay = Duration::from_secs(5);

            loop {
                match client.perform_login(&api_key, org_name.as_deref()).await {
                    Ok(()) => {
                        tracing::debug!("Background login completed successfully");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("Background login failed: {}, retrying in {:?}", e, delay);
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(max_delay);
                    }
                }
            }
        });
    }

    /// Wait for login state to be available.
    async fn wait_for_login_state(&self) -> Result<LoginState> {
        // Check if already logged in
        if let Some(state) = self.inner.login_state.read().await.clone() {
            return Ok(state);
        }

        // Get notification future BEFORE checking state to avoid race condition
        let notified = self.inner.login_notify.notified();

        // Check state again (may have been set between our first check and now)
        if let Some(state) = self.inner.login_state.read().await.clone() {
            return Ok(state);
        }

        // Wait for notification or timeout
        tokio::select! {
            _ = notified => {
                // Login completed - state is guaranteed to be set since
                // notify_waiters() is only called after setting state
                self.inner.login_state.read().await.clone().ok_or_else(|| {
                    BraintrustError::InvalidConfig(
                        "Login notification received but state not set".into(),
                    )
                })
            }
            _ = tokio::time::sleep(LOGIN_TIMEOUT) => {
                Err(BraintrustError::InvalidConfig(
                    "Timeout waiting for login to complete".into(),
                ))
            }
        }
    }

    /// Submit a span payload for logging (fire-and-forget).
    ///
    /// Returns immediately after queuing. HTTP submission happens in the background.
    /// Errors are logged as warnings but not propagated to callers.
    pub(crate) async fn submit_payload(
        &self,
        token: impl Into<String>,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        let cmd = LogCommand::Submit(Box::new(SubmitCommand {
            token: token.into(),
            payload,
            parent_info,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        Ok(())
    }

    /// Flush all pending log events.
    pub async fn flush(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .sender
            .send(LogCommand::Flush(tx))
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }

    /// Trigger a non-blocking background flush.
    /// Does not wait for completion - useful for streaming writes.
    pub async fn trigger_flush(&self) -> Result<()> {
        self.inner
            .sender
            .send(LogCommand::TriggerFlush)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)
    }
}

#[async_trait]
impl SpanSubmitter for BraintrustClient {
    async fn submit(
        &self,
        token: impl Into<String> + Send,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        self.submit_payload(token, payload, parent_info).await
    }

    async fn trigger_flush(&self) -> Result<()> {
        self.trigger_flush().await
    }
}

enum LogCommand {
    Submit(Box<SubmitCommand>),
    Flush(oneshot::Sender<std::result::Result<(), anyhow::Error>>),
    TriggerFlush,
}

struct SubmitCommand {
    token: String,
    payload: SpanPayload,
    parent_info: Option<ParentSpanInfo>,
}

/// Request body for project registration.
#[derive(Serialize)]
struct ProjectRegisterRequest<'a> {
    project_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    org_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    org_name: Option<&'a str>,
}

/// Response from project registration.
#[derive(Deserialize)]
struct ProjectRegisterResponse {
    project: ProjectInfo,
}

/// Project info in registration response.
#[derive(Deserialize)]
struct ProjectInfo {
    id: String,
}

async fn run_worker(
    api_url: Url,
    app_url: Url,
    mut receiver: mpsc::Receiver<LogCommand>,
    config: WorkerConfig,
) {
    let mut state = WorkerState::new(api_url, app_url, config);
    let mut flush_interval = tokio::time::interval(state.flush_interval);

    loop {
        tokio::select! {
            cmd = receiver.recv() => {
                match cmd {
                    Some(LogCommand::Submit(cmd)) => {
                        let SubmitCommand {
                            token,
                            payload,
                            parent_info,
                        } = *cmd;
                        // Queue row for batch submission (fire-and-forget)
                        if let Err(e) = state.prepare_and_queue_row(&token, payload, parent_info).await {
                            warn!(error = %e, "failed to queue span");
                        }
                    }
                    Some(LogCommand::Flush(response)) => {
                        // Flush all pending rows and respond
                        let result = state.flush_pending().await;
                        let _ = response.send(result);
                    }
                    Some(LogCommand::TriggerFlush) => {
                        // Non-blocking flush (fire-and-forget)
                        if let Err(e) = state.flush_pending().await {
                            warn!(error = %e, "background flush failed");
                        }
                    }
                    None => {
                        // Channel closed - flush remaining and exit
                        if let Err(e) = state.flush_pending().await {
                            warn!(error = %e, "final flush failed");
                        }
                        break;
                    }
                }
            }
            _ = flush_interval.tick() => {
                // Periodic flush per spec (default: 1 second)
                if !state.pending.is_empty() {
                    if let Err(e) = state.flush_pending().await {
                        warn!(error = %e, "periodic flush failed");
                    }
                }
            }
        }
    }
}

struct WorkerState {
    /// URL for data plane requests (e.g., /logs3)
    api_url: Url,
    /// URL for control plane requests (e.g., /api/project/register)
    app_url: Url,
    client: reqwest::Client,
    project_cache: IndexMap<String, String>,
    /// Pending rows awaiting batch submission, keyed for merging.
    /// Uses IndexMap to maintain insertion order for FIFO queue behavior.
    pending: IndexMap<RowKey, PendingRow>,
    /// Token associated with pending rows (all rows share the same token).
    pending_token: Option<String>,
    /// Interval between periodic flushes.
    flush_interval: Duration,
    /// Maximum items per HTTP batch.
    batch_max_items: usize,
    /// Maximum bytes per HTTP batch.
    batch_max_bytes: usize,
    /// Maximum queue size (None = unlimited).
    queue_max_size: Option<usize>,
    /// Count of dropped events (for logging).
    dropped_count: usize,
}

impl WorkerState {
    fn new(api_url: Url, app_url: Url, config: WorkerConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client");

        // Use config values, falling back to environment variables, then defaults.
        let flush_interval = config.flush_interval.unwrap_or(DEFAULT_FLUSH_INTERVAL);

        let batch_max_items = config.batch_max_items.unwrap_or_else(|| {
            std::env::var("BRAINTRUST_DEFAULT_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_BATCH_MAX_ITEMS)
        });

        let batch_max_bytes = config.batch_max_bytes.unwrap_or_else(|| {
            std::env::var("BRAINTRUST_MAX_REQUEST_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_BATCH_MAX_BYTES)
        });

        let queue_max_size = config.queue_max_size.or_else(|| {
            std::env::var("BRAINTRUST_QUEUE_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
        });

        Self {
            api_url,
            app_url,
            client,
            project_cache: IndexMap::new(),
            pending: IndexMap::new(),
            pending_token: None,
            flush_interval,
            batch_max_items,
            batch_max_bytes,
            queue_max_size,
            dropped_count: 0,
        }
    }

    /// Prepare a row from payload and queue it for batch submission.
    async fn prepare_and_queue_row(
        &mut self,
        token: &str,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> std::result::Result<(), anyhow::Error> {
        let SpanPayload {
            row_id,
            span_id,
            is_merge,
            org_id,
            org_name,
            project_name,
            input,
            output,
            expected,
            error,
            scores,
            metadata,
            metrics,
            tags,
            context,
            span_attributes,
        } = payload;

        let project_id = if let Some(ref project_name) = project_name {
            Some(
                self.ensure_project_id(token, &org_id, org_name.as_deref(), project_name)
                    .await?,
            )
        } else {
            None
        };

        // Determine destination and span hierarchy based on parent info
        let (root_span_id, span_parents, destination) = match parent_info {
            None => {
                // No parent - use project_id if available, otherwise fail
                let dest = match project_id {
                    Some(pid) => LogDestination::project_logs(pid),
                    None => {
                        anyhow::bail!("no destination: either parent_info or project_name required")
                    }
                };
                (span_id.clone(), None, dest)
            }
            Some(ParentSpanInfo::Experiment { object_id }) => {
                (span_id.clone(), None, LogDestination::experiment(object_id))
            }
            Some(ParentSpanInfo::ProjectLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::project_logs(object_id),
            ),
            Some(ParentSpanInfo::ProjectName { project_name }) => {
                let proj_id = self
                    .ensure_project_id(token, &org_id, org_name.as_deref(), &project_name)
                    .await?;
                (span_id.clone(), None, LogDestination::project_logs(proj_id))
            }
            Some(ParentSpanInfo::PlaygroundLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::playground_logs(object_id),
            ),
            Some(ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                span_id: parent_span_id,
                root_span_id: parent_root_span_id,
            }) => {
                let span_parents = Some(vec![parent_span_id]);
                let dest = match object_type {
                    SpanObjectType::Experiment => LogDestination::experiment(object_id),
                    SpanObjectType::ProjectLogs => LogDestination::project_logs(object_id),
                    SpanObjectType::PlaygroundLogs => LogDestination::playground_logs(object_id),
                };
                (parent_root_span_id, span_parents, dest)
            }
        };

        let row = Logs3Row {
            id: row_id,
            is_merge: if is_merge { Some(true) } else { None },
            span_id,
            root_span_id,
            span_parents,
            destination,
            org_id,
            org_name,
            input,
            output,
            expected,
            error,
            scores,
            metadata,
            metrics,
            tags,
            context,
            span_attributes,
            created: Utc::now(),
        };

        // Queue the row for merging and batch submission
        self.merge_row(row);
        self.pending_token = Some(token.to_string());
        self.enforce_queue_limit();

        Ok(())
    }

    /// Merge a row into pending queue (or replace if not a merge operation).
    fn merge_row(&mut self, new_row: Logs3Row) {
        let key = RowKey::from_row(&new_row);

        match self.pending.entry(key) {
            Entry::Occupied(mut entry) => {
                if new_row.is_merge.unwrap_or(false) {
                    // Deep merge into existing row
                    let pending = entry.get_mut();
                    merge_into(&mut pending.row, new_row);
                } else {
                    // Replace entirely
                    entry.insert(PendingRow { row: new_row });
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(PendingRow { row: new_row });
            }
        }
    }

    /// Enforce queue size limit by dropping oldest entries (FIFO).
    fn enforce_queue_limit(&mut self) {
        if let Some(max_size) = self.queue_max_size {
            while self.pending.len() > max_size {
                // Drop oldest entry (IndexMap maintains insertion order)
                if let Some(key) = self.pending.keys().next().cloned() {
                    self.pending.shift_remove(&key);
                    self.dropped_count += 1;
                    warn!(
                        "Queue full, dropping oldest event (total dropped: {})",
                        self.dropped_count
                    );
                }
            }
        }
    }

    /// Flush all pending rows in batches.
    async fn flush_pending(&mut self) -> std::result::Result<(), anyhow::Error> {
        if self.pending.is_empty() {
            return Ok(());
        }

        let token = match self.pending_token.take() {
            Some(t) => t,
            None => return Ok(()),
        };

        let rows: Vec<Logs3Row> = self
            .pending
            .drain(..)
            .map(|(_, pending)| pending.row)
            .collect();

        // Batch and serialize in one pass to avoid double serialization
        for batch in self.batch_and_serialize_rows(rows) {
            if let Err(e) = self.send_batch_bytes(&token, batch).await {
                warn!(error = %e, "batch send failed");
            }
        }

        Ok(())
    }

    /// Split rows into batches and serialize them, respecting size limits.
    /// This combines batching and serialization to avoid double JSON encoding.
    fn batch_and_serialize_rows(&self, rows: Vec<Logs3Row>) -> Vec<Vec<u8>> {
        let mut batches = Vec::new();
        let mut current_batch = Vec::new();
        let mut current_size = 0;

        for row in rows {
            // Estimate size using to_vec (will be actual serialization)
            let row_json = match serde_json::to_vec(&row) {
                Ok(json) => json,
                Err(e) => {
                    warn!(error = %e, "failed to serialize row, skipping");
                    continue;
                }
            };
            let row_size = row_json.len();

            if current_batch.len() >= self.batch_max_items
                || (current_size + row_size > self.batch_max_bytes && !current_batch.is_empty())
            {
                // Serialize current batch and start new one
                if let Ok(batch_bytes) = self.serialize_batch(&current_batch) {
                    batches.push(batch_bytes);
                }
                current_batch.clear();
                current_size = 0;
            }

            current_size += row_size;
            current_batch.push(row);
        }

        if !current_batch.is_empty() {
            if let Ok(batch_bytes) = self.serialize_batch(&current_batch) {
                batches.push(batch_bytes);
            }
        }

        batches
    }

    /// Serialize a batch of rows into the Logs3Request format.
    fn serialize_batch(&self, rows: &[Logs3Row]) -> std::result::Result<Vec<u8>, anyhow::Error> {
        let request = Logs3Request {
            rows: rows.to_vec(),
            api_version: LOGS_API_VERSION,
        };
        serde_json::to_vec(&request).map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))
    }

    /// Send pre-serialized batch bytes with retry logic.
    async fn send_batch_bytes(
        &self,
        token: &str,
        json_bytes: Vec<u8>,
    ) -> std::result::Result<(), anyhow::Error> {
        let logs_url = self
            .api_url
            .join("logs3")
            .map_err(|e| anyhow::anyhow!("invalid logs url: {e}"))?;

        // Retry with exponential backoff per spec
        let mut delay = INITIAL_RETRY_DELAY;

        for attempt in 0..DEFAULT_MAX_RETRIES {
            let response = self
                .client
                .post(logs_url.clone())
                .bearer_auth(token)
                .header("content-type", "application/json")
                .body(json_bytes.clone())
                .send()
                .await;

            match response {
                Ok(resp) if resp.status().is_success() => {
                    return Ok(());
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "<unavailable>".to_string());

                    // Check if retryable per spec:
                    // - 5xx server errors: retryable
                    // - 429 rate limit: retryable
                    // - 4xx client errors (except 429): NOT retryable
                    let is_retryable = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || status.is_server_error();

                    if is_retryable && attempt < DEFAULT_MAX_RETRIES - 1 {
                        warn!(
                            status = %status,
                            attempt, "batch send failed with retryable error, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                        continue;
                    }

                    anyhow::bail!("batch send failed: [{}] {}", status, body);
                }
                Err(e) => {
                    // Network error - retryable
                    if attempt < DEFAULT_MAX_RETRIES - 1 {
                        warn!(error = %e, attempt, "batch send network error, retrying");
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }

        unreachable!("retry loop should always return or continue")
    }

    async fn ensure_project_id(
        &mut self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        project_name: &str,
    ) -> std::result::Result<String, anyhow::Error> {
        let cache_key = format!("{org_id}:{project_name}");
        if let Some(project_id) = self.project_cache.get(&cache_key) {
            return Ok(project_id.clone());
        }

        let request = ProjectRegisterRequest {
            project_name,
            org_id: (!org_id.is_empty()).then_some(org_id),
            org_name,
        };

        let url = self
            .app_url
            .join("api/project/register")
            .map_err(|e| anyhow::anyhow!("invalid project register url: {e}"))?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("register project failed: [{status}] {text}");
        }

        let register_response: ProjectRegisterResponse = response
            .json()
            .await
            .context("failed to parse project registration response")?;

        self.project_cache
            .insert(cache_key, register_response.project.id.clone());
        Ok(register_response.project.id)
    }
}

/// Deep merge source row into target row.
/// Matches the behavior of TS SDK's mergeRowBatch and Brainstore's WAL merge logic.
/// - JSON fields (input, output, expected, error): deep merged recursively
/// - Map fields (metadata, metrics): keys merged, last write wins for conflicts
/// - Identity fields (created, span_id, root_span_id, span_parents): preserved from target
fn merge_into(target: &mut Logs3Row, source: Logs3Row) {
    // Deep merge JSON fields (input, output, expected, error)
    if let Some(source_input) = source.input {
        match &mut target.input {
            Some(target_input) => deep_merge(target_input, &source_input),
            None => target.input = Some(source_input),
        }
    }
    if let Some(source_output) = source.output {
        match &mut target.output {
            Some(target_output) => deep_merge(target_output, &source_output),
            None => target.output = Some(source_output),
        }
    }
    if let Some(source_expected) = source.expected {
        match &mut target.expected {
            Some(target_expected) => deep_merge(target_expected, &source_expected),
            None => target.expected = Some(source_expected),
        }
    }
    if let Some(source_error) = source.error {
        match &mut target.error {
            Some(target_error) => deep_merge(target_error, &source_error),
            None => target.error = Some(source_error),
        }
    }

    // Deep merge context (JSON field)
    if let Some(source_context) = source.context {
        match &mut target.context {
            Some(target_context) => deep_merge(target_context, &source_context),
            None => target.context = Some(source_context),
        }
    }

    // Merge scores map (last write wins for conflicts)
    if let Some(source_scores) = source.scores {
        let target_scores = target
            .scores
            .get_or_insert_with(std::collections::HashMap::new);
        for (k, v) in source_scores {
            target_scores.insert(k, v);
        }
    }

    // Deep merge metadata map (last write wins for conflicts)
    if let Some(source_meta) = source.metadata {
        let target_meta = target.metadata.get_or_insert_with(Map::new);
        for (k, v) in source_meta {
            target_meta.insert(k, v);
        }
    }

    // Deep merge metrics map (last write wins for conflicts)
    if let Some(source_metrics) = source.metrics {
        let target_metrics = target
            .metrics
            .get_or_insert_with(std::collections::HashMap::new);
        for (k, v) in source_metrics {
            target_metrics.insert(k, v);
        }
    }

    // Merge tags (deduplicate by converting to set and back)
    if let Some(source_tags) = source.tags {
        match &mut target.tags {
            Some(target_tags) => {
                // Add new tags, avoiding duplicates
                for tag in source_tags {
                    if !target_tags.contains(&tag) {
                        target_tags.push(tag);
                    }
                }
            }
            None => target.tags = Some(source_tags),
        }
    }

    // Deep merge span_attributes
    if let Some(source_attrs) = source.span_attributes {
        let target_attrs = target
            .span_attributes
            .get_or_insert_with(SpanAttributes::default);
        if source_attrs.name.is_some() {
            target_attrs.name = source_attrs.name;
        }
        if source_attrs.span_type.is_some() {
            target_attrs.span_type = source_attrs.span_type;
        }
        if source_attrs.purpose.is_some() {
            target_attrs.purpose = source_attrs.purpose;
        }
        for (k, v) in source_attrs.extra {
            target_attrs.extra.insert(k, v);
        }
    }

    // Merge org_name if source has it
    if source.org_name.is_some() {
        target.org_name = source.org_name;
    }

    // Note: Identity fields (created, span_id, root_span_id, span_parents) are NOT merged.
    // These are auto-generated attributes that should be preserved from the first row.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanLog;
    use serde_json::Value;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Helper to create a mock login response
    fn mock_login_response(orgs: &[(&str, &str)]) -> ResponseTemplate {
        let org_info: Vec<_> = orgs
            .iter()
            .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
            .collect();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "org_info": org_info }))
    }

    #[tokio::test]
    async fn builder_rejects_missing_api_key() {
        // Clear any env vars that might be set
        std::env::remove_var("BRAINTRUST_API_KEY");

        let result = BraintrustClient::builder()
            .app_url("https://example.com")
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn builder_rejects_invalid_app_url() {
        let result = BraintrustClient::builder()
            .api_key("test-key")
            .app_url("::not a url::")
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn builder_rejects_invalid_api_url() {
        let result = BraintrustClient::builder()
            .api_key("test-key")
            .api_url("::not a url::")
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn project_registration_is_cached() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "test-project-id" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-id", "Test Org")]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("token")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        for _ in 0..2 {
            let span = client
                .span_builder()
                .await
                .expect("span_builder")
                .project_name("demo-project")
                .build();
            span.log(
                SpanLog::builder()
                    .input(Value::String("hello".into()))
                    .build()
                    .expect("build"),
            )
            .await;
            span.flush().await.expect("flush");
            client.flush().await.expect("client flush");
        }

        let register_calls = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/api/project/register")
            .count();

        assert_eq!(register_calls, 1);
    }

    #[tokio::test]
    async fn logs_request_contains_span_rows() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-id", "Test Org")]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("token")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let span = client
            .span_builder()
            .await
            .expect("span_builder")
            .project_name("demo-project")
            .build();
        span.log(
            SpanLog::builder()
                .input(Value::String("input".into()))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        assert!(body.get("rows").is_some());
    }

    #[tokio::test]
    async fn blocking_login_returns_first_org_by_default() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[
                ("org-1", "First Org"),
                ("org-2", "Second Org"),
            ]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let login_state = client.login_state().await.expect("should be logged in");

        assert_eq!(login_state.org_id, "org-1");
        assert_eq!(login_state.org_name, "First Org");
        assert_eq!(login_state.api_key, "test-api-key");
    }

    #[tokio::test]
    async fn blocking_login_selects_org_by_name() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[
                ("org-1", "First Org"),
                ("org-2", "Second Org"),
            ]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .org_name("Second Org")
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let login_state = client.login_state().await.expect("should be logged in");

        assert_eq!(login_state.org_id, "org-2");
        assert_eq!(login_state.org_name, "Second Org");
    }

    #[tokio::test]
    async fn blocking_login_errors_when_org_not_found() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-1", "First Org")]))
            .mount(&server)
            .await;

        let result = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .org_name("Nonexistent Org")
            .blocking_login(true)
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn blocking_login_errors_when_no_orgs_returned() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "org_info": [] })),
            )
            .mount(&server)
            .await;

        let result = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn blocking_login_errors_on_api_failure() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let result = BraintrustClient::builder()
            .api_key("bad-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::Api { status: 401, .. }));
    }

    #[tokio::test]
    async fn is_logged_in_returns_false_initially_with_background_login() {
        let server = MockServer::start().await;

        // Don't mount any mock - login will fail and retry in background
        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            // blocking_login defaults to false
            .build()
            .await
            .expect("client");

        // Initially should not be logged in (background login hasn't completed)
        let is_logged_in = client.is_logged_in().await;
        assert!(!is_logged_in);
    }

    #[tokio::test]
    async fn wait_for_login_succeeds_after_background_login() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-1", "Test Org")]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .build()
            .await
            .expect("client");

        // Wait for background login to complete
        let login_state = client.wait_for_login().await.expect("login should succeed");
        assert_eq!(login_state.org_id, "org-1");
        assert!(client.is_logged_in().await);
    }

    #[tokio::test]
    async fn span_builder_uses_login_state_and_default_project() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-123", "Test Org")]))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .default_project("test-project")
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        // span_builder() should use login state and default project
        let span = client.span_builder().await.expect("span_builder").build();

        span.log(SpanLog {
            input: Some(Value::String("test".into())),
            ..Default::default()
        })
        .await;
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify the logs request was made with the correct org_id
        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        let rows = body.get("rows").and_then(|r| r.as_array()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("org_id").and_then(|v| v.as_str()),
            Some("org-123")
        );
    }

    #[tokio::test]
    async fn span_builder_with_credentials_bypasses_login() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        // Create client without any login mock - background login will fail
        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .build()
            .await
            .expect("client");

        // Use span_builder_with_credentials to bypass login
        let span = client
            .span_builder_with_credentials("explicit-token", "explicit-org-id")
            .project_name("demo-project")
            .build();

        span.log(SpanLog {
            input: Some(Value::String("test".into())),
            ..Default::default()
        })
        .await;
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify the logs request was made with the explicit org_id
        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        let rows = body.get("rows").and_then(|r| r.as_array()).unwrap();
        assert_eq!(
            rows[0].get("org_id").and_then(|v| v.as_str()),
            Some("explicit-org-id")
        );
    }

    #[tokio::test]
    async fn separate_urls_for_data_and_control_plane() {
        // Use two separate mock servers to verify requests go to the correct URLs
        let api_server = MockServer::start().await; // Data plane: /logs3
        let app_server = MockServer::start().await; // Control plane: /api/project/register, /api/apikey/login

        // Mount login mock on app_server
        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-id", "Test Org")]))
            .mount(&app_server)
            .await;

        // Mount project registration on app_server (control plane)
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        // Mount logs endpoint on api_server (data plane)
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&api_server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(app_server.uri()) // Control plane
            .api_url(api_server.uri()) // Data plane
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let span = client
            .span_builder()
            .await
            .expect("span_builder")
            .project_name("test-project")
            .build();

        span.log(SpanLog {
            input: Some(Value::String("test".into())),
            ..Default::default()
        })
        .await;
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify api_server received the /logs3 request
        let api_requests = api_server.received_requests().await.unwrap();
        assert_eq!(api_requests.len(), 1);
        assert_eq!(api_requests[0].url.path(), "/logs3");

        // Verify app_server received the /api/project/register request (and login)
        let app_requests = app_server.received_requests().await.unwrap();
        let register_requests: Vec<_> = app_requests
            .iter()
            .filter(|r| r.url.path() == "/api/project/register")
            .collect();
        assert_eq!(register_requests.len(), 1);

        // Verify app_server did NOT receive /logs3
        let logs_on_app: Vec<_> = app_requests
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .collect();
        assert!(logs_on_app.is_empty(), "/logs3 should not go to app_server");

        // Verify api_server did NOT receive /api/project/register
        let register_on_api: Vec<_> = api_requests
            .iter()
            .filter(|r| r.url.path() == "/api/project/register")
            .collect();
        assert!(
            register_on_api.is_empty(),
            "/api/project/register should not go to api_server"
        );
    }

    #[test]
    fn row_key_from_experiment_row() {
        let row = Logs3Row {
            id: "row-1".to_string(),
            is_merge: None,
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: None,
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
        };

        let key = RowKey::from_row(&row);
        assert_eq!(key.org_id, "org-1");
        assert_eq!(key.experiment_id, Some("exp-123".to_string()));
        assert_eq!(key.project_id, None);
        assert_eq!(key.dataset_id, None);
        assert_eq!(key.prompt_session_id, None);
        assert_eq!(key.log_id, None);
        assert_eq!(key.row_id, "row-1");
    }

    #[test]
    fn row_key_from_project_logs_row() {
        let row = Logs3Row {
            id: "row-2".to_string(),
            is_merge: None,
            span_id: "span-2".to_string(),
            root_span_id: "root-2".to_string(),
            span_parents: None,
            destination: LogDestination::project_logs("proj-456"),
            org_id: "org-2".to_string(),
            org_name: None,
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
        };

        let key = RowKey::from_row(&row);
        assert_eq!(key.org_id, "org-2");
        assert_eq!(key.experiment_id, None);
        assert_eq!(key.project_id, Some("proj-456".to_string()));
        assert_eq!(key.dataset_id, None);
        assert_eq!(key.prompt_session_id, None);
        assert_eq!(key.log_id, Some("g".to_string()));
        assert_eq!(key.row_id, "row-2");
    }

    #[test]
    fn merge_into_overwrites_scalar_fields() {
        let mut target = Logs3Row {
            id: "row-1".to_string(),
            is_merge: None,
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: None,
            input: Some(serde_json::json!("old-input")),
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
        };

        let source = Logs3Row {
            id: "row-1".to_string(),
            is_merge: Some(true),
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: Some("new-org-name".to_string()),
            input: Some(serde_json::json!("new-input")),
            output: Some(serde_json::json!("new-output")),
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
        };

        merge_into(&mut target, source);

        assert_eq!(target.input, Some(serde_json::json!("new-input")));
        assert_eq!(target.output, Some(serde_json::json!("new-output")));
        assert_eq!(target.org_name, Some("new-org-name".to_string()));
    }

    #[test]
    fn merge_into_deep_merges_metadata() {
        let mut target = Logs3Row {
            id: "row-1".to_string(),
            is_merge: None,
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: None,
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: Some(
                [("key1".to_string(), serde_json::json!("value1"))]
                    .into_iter()
                    .collect(),
            ),
            metrics: Some([("metric1".to_string(), 1.0)].into_iter().collect()),
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
        };

        let source = Logs3Row {
            id: "row-1".to_string(),
            is_merge: Some(true),
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: None,
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: Some(
                [("key2".to_string(), serde_json::json!("value2"))]
                    .into_iter()
                    .collect(),
            ),
            metrics: Some([("metric2".to_string(), 2.0)].into_iter().collect()),
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
        };

        merge_into(&mut target, source);

        let metadata = target.metadata.unwrap();
        assert_eq!(metadata.get("key1").unwrap(), "value1");
        assert_eq!(metadata.get("key2").unwrap(), "value2");

        let metrics = target.metrics.unwrap();
        assert_eq!(metrics.get("metric1").copied(), Some(1.0));
        assert_eq!(metrics.get("metric2").copied(), Some(2.0));
    }

    #[test]
    fn batching_respects_item_limit() {
        let state = WorkerState::new(
            Url::parse("https://api.braintrust.dev").expect("valid url"),
            Url::parse("https://www.braintrust.dev").expect("valid url"),
            WorkerConfig::default(),
        );

        // Create more rows than batch limit
        let rows: Vec<Logs3Row> = (0..250)
            .map(|i| Logs3Row {
                id: format!("row-{}", i),
                is_merge: None,
                span_id: format!("span-{}", i),
                root_span_id: format!("root-{}", i),
                span_parents: None,
                destination: LogDestination::experiment("exp-123"),
                org_id: "org-1".to_string(),
                org_name: None,
                input: None,
                output: None,
                expected: None,
                error: None,
                scores: None,
                metadata: None,
                metrics: None,
                tags: None,
                context: None,
                span_attributes: None,
                created: Utc::now(),
            })
            .collect();

        let batches = state.batch_and_serialize_rows(rows);

        // Should create 3 batches: 100, 100, 50
        assert_eq!(batches.len(), 3);

        // Verify batches are non-empty and serialized correctly
        for batch_bytes in &batches {
            assert!(!batch_bytes.is_empty(), "batch should not be empty");
            // Verify it's valid JSON
            let value: Value =
                serde_json::from_slice(batch_bytes).expect("batch should be valid JSON");
            assert!(value.get("rows").is_some(), "batch should have rows field");
            assert!(
                value.get("api_version").is_some(),
                "batch should have api_version field"
            );
        }
    }
}
