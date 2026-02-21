use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot, Notify, RwLock};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

use crate::dataset::{
    BTQLQuery, BTQLResponse, DatasetBuilder, DatasetFetcher, DatasetRegisterRequest,
    DatasetRegisterResponse, DatasetRegistrar, DatasetSummarizer, DatasetSummaryResponse,
};
use crate::error::{BraintrustError, Result};
use crate::experiment::{
    BaseExperimentFetcher, BaseExperimentInfo, BaseExperimentRequest, BaseExperimentResponse,
    ExperimentBuilder, ExperimentComparisonFetcher, ExperimentComparisonResponse,
    ExperimentRegisterRequest, ExperimentRegisterResponse, ExperimentRegistrar,
};
use crate::span::SpanSubmitter;
use crate::types::{
    LogDestination, Logs3Request, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload,
    LOGS_API_VERSION,
};

const DEFAULT_QUEUE_SIZE: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

/// Organization info returned from login.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub api_url: Option<String>,
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
        }
    }

    /// Set the API key (overrides `BRAINTRUST_API_KEY` env var).
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
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

        let (sender, receiver) = mpsc::channel(self.queue_size.max(32));
        let worker = tokio::spawn(run_worker(api_url.clone(), app_url.clone(), receiver));

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
            crate::span::SpanBuilder::new(Arc::new(self.clone()), &state.api_key, &state.org_name);
        if let Some(ref project) = self.inner.default_project {
            builder = builder.project_name(project);
        }
        Ok(builder)
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

    /// Create an experiment builder using the logged-in state and default project.
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
    /// let experiment = client
    ///     .experiment_builder()
    ///     .await?
    ///     .experiment_name("baseline")
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn experiment_builder(&self) -> Result<ExperimentBuilder<Self>> {
        let state = self.wait_for_login_state().await?;
        let mut builder =
            ExperimentBuilder::new(Arc::new(self.clone()), &state.api_key, &state.org_name);
        if let Some(ref project) = self.inner.default_project {
            builder = builder.project_name(project);
        }
        Ok(builder)
    }

    /// Create a dataset builder using the logged-in state and default project.
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
    /// let dataset = client
    ///     .dataset_builder()
    ///     .await?
    ///     .dataset_name("test-cases")
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn dataset_builder(&self) -> Result<DatasetBuilder<Self>> {
        let state = self.wait_for_login_state().await?;
        let mut builder =
            DatasetBuilder::new(Arc::new(self.clone()), &state.api_key, &state.org_name);
        if let Some(ref project) = self.inner.default_project {
            builder = builder.project_name(project);
        }
        Ok(builder)
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
}

#[async_trait]
impl ExperimentRegistrar for BraintrustClient {
    async fn register_experiment(
        &self,
        token: &str,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse> {
        let (tx, rx) = oneshot::channel();
        let cmd = LogCommand::RegisterExperiment(Box::new(RegisterExperimentCommand {
            token: token.to_string(),
            request,
            response: tx,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

#[async_trait]
impl BaseExperimentFetcher for BraintrustClient {
    async fn fetch_base_experiment(
        &self,
        token: &str,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>> {
        let (tx, rx) = oneshot::channel();
        let cmd = LogCommand::FetchBaseExperiment(Box::new(FetchBaseExperimentCommand {
            token: token.to_string(),
            experiment_id: experiment_id.to_string(),
            response: tx,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

#[async_trait]
impl ExperimentComparisonFetcher for BraintrustClient {
    async fn fetch_experiment_comparison(
        &self,
        token: &str,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse> {
        let (tx, rx) = oneshot::channel();
        let cmd =
            LogCommand::FetchExperimentComparison(Box::new(FetchExperimentComparisonCommand {
                token: token.to_string(),
                experiment_id: experiment_id.to_string(),
                base_experiment_id: base_experiment_id.map(String::from),
                response: tx,
            }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

#[async_trait]
impl DatasetRegistrar for BraintrustClient {
    async fn register_dataset(
        &self,
        token: &str,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse> {
        let (tx, rx) = oneshot::channel();
        let cmd = LogCommand::RegisterDataset(Box::new(RegisterDatasetCommand {
            token: token.to_string(),
            request,
            response: tx,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

#[async_trait]
impl DatasetFetcher for BraintrustClient {
    async fn fetch_dataset_records(&self, token: &str, query: BTQLQuery) -> Result<BTQLResponse> {
        let (tx, rx) = oneshot::channel();
        let cmd = LogCommand::FetchDatasetRecords(Box::new(FetchDatasetRecordsCommand {
            token: token.to_string(),
            query,
            response: tx,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

#[async_trait]
impl DatasetSummarizer for BraintrustClient {
    async fn fetch_dataset_summary(
        &self,
        token: &str,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse> {
        let (tx, rx) = oneshot::channel();
        let cmd = LogCommand::FetchDatasetSummary(Box::new(FetchDatasetSummaryCommand {
            token: token.to_string(),
            dataset_id: dataset_id.to_string(),
            response: tx,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

enum LogCommand {
    Submit(Box<SubmitCommand>),
    Flush(oneshot::Sender<std::result::Result<(), anyhow::Error>>),
    /// Register an experiment.
    RegisterExperiment(Box<RegisterExperimentCommand>),
    /// Fetch base experiment for comparison.
    FetchBaseExperiment(Box<FetchBaseExperimentCommand>),
    /// Fetch experiment comparison data.
    FetchExperimentComparison(Box<FetchExperimentComparisonCommand>),
    /// Register a dataset.
    RegisterDataset(Box<RegisterDatasetCommand>),
    /// Fetch dataset records via BTQL.
    FetchDatasetRecords(Box<FetchDatasetRecordsCommand>),
    /// Fetch dataset summary.
    FetchDatasetSummary(Box<FetchDatasetSummaryCommand>),
}

struct RegisterExperimentCommand {
    token: String,
    request: ExperimentRegisterRequest,
    response: oneshot::Sender<std::result::Result<ExperimentRegisterResponse, anyhow::Error>>,
}

struct FetchBaseExperimentCommand {
    token: String,
    experiment_id: String,
    response: oneshot::Sender<std::result::Result<Option<BaseExperimentInfo>, anyhow::Error>>,
}

struct FetchExperimentComparisonCommand {
    token: String,
    experiment_id: String,
    base_experiment_id: Option<String>,
    response: oneshot::Sender<std::result::Result<ExperimentComparisonResponse, anyhow::Error>>,
}

struct RegisterDatasetCommand {
    token: String,
    request: DatasetRegisterRequest,
    response: oneshot::Sender<std::result::Result<DatasetRegisterResponse, anyhow::Error>>,
}

struct FetchDatasetRecordsCommand {
    token: String,
    query: BTQLQuery,
    response: oneshot::Sender<std::result::Result<BTQLResponse, anyhow::Error>>,
}

struct FetchDatasetSummaryCommand {
    token: String,
    dataset_id: String,
    response: oneshot::Sender<std::result::Result<DatasetSummaryResponse, anyhow::Error>>,
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

async fn run_worker(api_url: Url, app_url: Url, mut receiver: mpsc::Receiver<LogCommand>) {
    let mut state = WorkerState::new(api_url, app_url);
    while let Some(cmd) = receiver.recv().await {
        match cmd {
            LogCommand::Submit(cmd) => {
                let SubmitCommand {
                    token,
                    payload,
                    parent_info,
                } = *cmd;
                // Fire-and-forget: log errors but don't propagate
                if let Err(e) = state.submit_payload(&token, payload, parent_info).await {
                    warn!(error = %e, "failed to submit span to Braintrust");
                }
            }
            LogCommand::Flush(response) => {
                let _ = response.send(Ok(()));
            }
            LogCommand::RegisterExperiment(cmd) => {
                let RegisterExperimentCommand {
                    token,
                    request,
                    response,
                } = *cmd;
                let result = state.register_experiment(&token, request).await;
                let _ = response.send(result);
            }
            LogCommand::FetchBaseExperiment(cmd) => {
                let FetchBaseExperimentCommand {
                    token,
                    experiment_id,
                    response,
                } = *cmd;
                let result = state.fetch_base_experiment(&token, &experiment_id).await;
                let _ = response.send(result);
            }
            LogCommand::FetchExperimentComparison(cmd) => {
                let FetchExperimentComparisonCommand {
                    token,
                    experiment_id,
                    base_experiment_id,
                    response,
                } = *cmd;
                let result = state
                    .fetch_experiment_comparison(
                        &token,
                        &experiment_id,
                        base_experiment_id.as_deref(),
                    )
                    .await;
                let _ = response.send(result);
            }
            LogCommand::RegisterDataset(cmd) => {
                let RegisterDatasetCommand {
                    token,
                    request,
                    response,
                } = *cmd;
                let result = state.register_dataset(&token, request).await;
                let _ = response.send(result);
            }
            LogCommand::FetchDatasetRecords(cmd) => {
                let FetchDatasetRecordsCommand {
                    token,
                    query,
                    response,
                } = *cmd;
                let result = state.fetch_dataset_records(&token, query).await;
                let _ = response.send(result);
            }
            LogCommand::FetchDatasetSummary(cmd) => {
                let FetchDatasetSummaryCommand {
                    token,
                    dataset_id,
                    response,
                } = *cmd;
                let result = state.fetch_dataset_summary(&token, &dataset_id).await;
                let _ = response.send(result);
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
    project_cache: HashMap<String, String>,
}

impl WorkerState {
    fn new(api_url: Url, app_url: Url) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client");
        Self {
            api_url,
            app_url,
            client,
            project_cache: HashMap::new(),
        }
    }

    async fn submit_payload(
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
        let mut event_fields = MutableSpanEvent {
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
            extra: HashMap::new(),
        };

        let project_id = if let Some(ref project_name) = project_name {
            Some(
                self.ensure_project_id(token, &org_id, org_name.as_deref(), project_name)
                    .await?,
            )
        } else {
            None
        };

        let logs_url = self
            .api_url
            .join("logs3")
            .map_err(|e| anyhow::anyhow!("invalid logs url: {e}"))?;

        // row_id and span_id come from payload - generated once at span creation, reused on every flush

        // Determine destination and span hierarchy based on parent info
        let (root_span_id, span_parents, destination, propagated_event) = match parent_info {
            None => {
                // No parent - use project_id if available, otherwise fail
                let dest = match project_id {
                    Some(pid) => LogDestination::project_logs(pid),
                    None => {
                        anyhow::bail!("no destination: either parent_info or project_name required")
                    }
                };
                (span_id.clone(), None, dest, None)
            }
            Some(ParentSpanInfo::Experiment { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::experiment(object_id),
                None,
            ),
            Some(ParentSpanInfo::ProjectLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::project_logs(object_id),
                None,
            ),
            Some(ParentSpanInfo::ProjectName { project_name }) => {
                let proj_id = self
                    .ensure_project_id(token, &org_id, org_name.as_deref(), &project_name)
                    .await?;
                (
                    span_id.clone(),
                    None,
                    LogDestination::project_logs(proj_id),
                    None,
                )
            }
            Some(ParentSpanInfo::PlaygroundLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::playground_logs(object_id),
                None,
            ),
            Some(ParentSpanInfo::Dataset { object_id }) => {
                (span_id.clone(), None, LogDestination::dataset(object_id))
            }
            Some(ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                span_id: parent_span_id,
                root_span_id: parent_root_span_id,
                propagated_event,
            }) => {
                let span_parents = Some(vec![parent_span_id]);
                let dest = match object_type {
                    SpanObjectType::Experiment => LogDestination::experiment(object_id),
                    SpanObjectType::ProjectLogs => LogDestination::project_logs(object_id),
                    SpanObjectType::PlaygroundLogs => LogDestination::playground_logs(object_id),
                };
                (parent_root_span_id, span_parents, dest, propagated_event)
            }
        };
        if let Some(propagated_event) = propagated_event {
            apply_propagated_event(&propagated_event, &mut event_fields);
        }

        let MutableSpanEvent {
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
            extra,
        } = event_fields;

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
            extra,
            created: Utc::now(),
        };

        let request = Logs3Request {
            rows: vec![row],
            api_version: LOGS_API_VERSION,
        };

        let json_bytes = serde_json::to_vec(&request)
            .map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))?;

        let response = self
            .client
            .post(logs_url)
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body(json_bytes)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable>".to_string());
            tracing::warn!("failed to submit span: [{status}] {body}");
        }

        Ok(())
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

    /// Register an experiment with the Braintrust API.
    async fn register_experiment(
        &self,
        token: &str,
        request: ExperimentRegisterRequest,
    ) -> std::result::Result<ExperimentRegisterResponse, anyhow::Error> {
        let url = self
            .api_url
            .join("api/experiment/register")
            .map_err(|e| anyhow::anyhow!("invalid experiment register url: {e}"))?;

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
            anyhow::bail!("register experiment failed: [{status}] {text}");
        }

        let register_response: ExperimentRegisterResponse = response
            .json()
            .await
            .context("failed to parse experiment registration response")?;

        Ok(register_response)
    }

    /// Fetch the base experiment for comparison.
    async fn fetch_base_experiment(
        &self,
        token: &str,
        experiment_id: &str,
    ) -> std::result::Result<Option<BaseExperimentInfo>, anyhow::Error> {
        let url = self
            .api_url
            .join("api/base_experiment/get_id")
            .map_err(|e| anyhow::anyhow!("invalid base experiment url: {e}"))?;

        let request = BaseExperimentRequest {
            id: experiment_id.to_string(),
        };

        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?;

        let status = response.status();

        // 400 means no base experiment - return None
        if status.as_u16() == 400 {
            return Ok(None);
        }

        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("fetch base experiment failed: [{status}] {text}");
        }

        let base_response: BaseExperimentResponse = response
            .json()
            .await
            .context("failed to parse base experiment response")?;

        match (base_response.base_exp_id, base_response.base_exp_name) {
            (Some(id), Some(name)) => Ok(Some(BaseExperimentInfo { id, name })),
            _ => Ok(None),
        }
    }

    /// Fetch experiment comparison data from the API.
    async fn fetch_experiment_comparison(
        &self,
        token: &str,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> std::result::Result<ExperimentComparisonResponse, anyhow::Error> {
        let mut url = self
            .api_url
            .join("experiment-comparison2")
            .map_err(|e| anyhow::anyhow!("invalid experiment comparison url: {e}"))?;

        url.query_pairs_mut()
            .append_pair("experiment_id", experiment_id);

        if let Some(base_id) = base_experiment_id {
            url.query_pairs_mut()
                .append_pair("base_experiment_id", base_id);
        }

        let response = self.client.get(url).bearer_auth(token).send().await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("fetch experiment comparison failed: [{status}] {text}");
        }

        let comparison: ExperimentComparisonResponse = response
            .json()
            .await
            .context("failed to parse experiment comparison response")?;

        Ok(comparison)
    }

    /// Register a dataset with the Braintrust API.
    async fn register_dataset(
        &self,
        token: &str,
        request: DatasetRegisterRequest,
    ) -> std::result::Result<DatasetRegisterResponse, anyhow::Error> {
        let url = self
            .api_url
            .join("api/dataset/register")
            .map_err(|e| anyhow::anyhow!("invalid dataset register url: {e}"))?;

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
            anyhow::bail!("register dataset failed: [{status}] {text}");
        }

        let register_response: DatasetRegisterResponse = response
            .json()
            .await
            .context("failed to parse dataset registration response")?;

        Ok(register_response)
    }

    /// Fetch dataset records via BTQL query.
    async fn fetch_dataset_records(
        &self,
        token: &str,
        query: BTQLQuery,
    ) -> std::result::Result<BTQLResponse, anyhow::Error> {
        let url = self
            .api_url
            .join("btql")
            .map_err(|e| anyhow::anyhow!("invalid btql url: {e}"))?;

        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&query)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("btql query failed: [{status}] {text}");
        }

        let btql_response: BTQLResponse = response
            .json()
            .await
            .context("failed to parse btql response")?;

        Ok(btql_response)
    }

    /// Fetch dataset summary.
    async fn fetch_dataset_summary(
        &self,
        token: &str,
        dataset_id: &str,
    ) -> std::result::Result<DatasetSummaryResponse, anyhow::Error> {
        let mut url = self
            .api_url
            .join("dataset-summary")
            .map_err(|e| anyhow::anyhow!("invalid dataset summary url: {e}"))?;

        url.query_pairs_mut().append_pair("dataset_id", dataset_id);

        let response = self.client.get(url).bearer_auth(token).send().await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("fetch dataset summary failed: [{status}] {text}");
        }

        let summary: DatasetSummaryResponse = response
            .json()
            .await
            .context("failed to parse dataset summary response")?;

        Ok(summary)
    }
}

#[derive(Debug, Default)]
struct MutableSpanEvent {
    input: Option<Value>,
    output: Option<Value>,
    expected: Option<Value>,
    error: Option<Value>,
    scores: Option<HashMap<String, f64>>,
    metadata: Option<Map<String, Value>>,
    metrics: Option<HashMap<String, f64>>,
    tags: Option<Vec<String>>,
    context: Option<Value>,
    span_attributes: Option<crate::types::SpanAttributes>,
    extra: HashMap<String, Value>,
}

#[derive(Serialize)]
struct SerializableSpanEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scores: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_attributes: Option<Value>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

impl MutableSpanEvent {
    fn to_event_map(&self) -> Map<String, Value> {
        let serialized = SerializableSpanEvent {
            input: self.input.clone(),
            output: self.output.clone(),
            expected: self.expected.clone(),
            error: self.error.clone(),
            scores: self.scores.as_ref().map(numeric_map_to_value),
            metadata: self.metadata.clone(),
            metrics: self.metrics.as_ref().map(numeric_map_to_value),
            tags: self.tags.clone(),
            context: self.context.clone(),
            span_attributes: self
                .span_attributes
                .clone()
                .and_then(|value| serde_json::to_value(value).ok()),
            extra: self.extra.clone(),
        };
        serde_json::to_value(serialized)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    fn apply_merged_event_map(&mut self, mut event: Map<String, Value>) {
        self.input = event.remove("input");
        self.output = event.remove("output");
        self.expected = event.remove("expected");
        self.error = event.remove("error");
        self.scores = event.remove("scores").and_then(parse_numeric_map);
        self.metadata = event.remove("metadata").and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        });
        self.metrics = event.remove("metrics").and_then(parse_numeric_map);
        self.tags = event.remove("tags").and_then(parse_string_array);
        self.context = event.remove("context");
        self.span_attributes = event
            .remove("span_attributes")
            .and_then(|value| serde_json::from_value(value).ok());
        self.extra = HashMap::from_iter(event);
    }
}

fn numeric_map_to_value(map: &HashMap<String, f64>) -> Value {
    Value::Object(Map::from_iter(
        map.iter()
            .map(|(key, value)| (key.clone(), Value::from(*value))),
    ))
}

fn apply_propagated_event(
    propagated_event: &Map<String, Value>,
    event_fields: &mut MutableSpanEvent,
) {
    let mut event = event_fields.to_event_map();
    merge_event_maps(&mut event, propagated_event);
    event_fields.apply_merged_event_map(event);
}

fn merge_event_maps(merge_into: &mut Map<String, Value>, merge_from: &Map<String, Value>) {
    for (key, merge_from_value) in merge_from {
        match (merge_into.get_mut(key), merge_from_value) {
            (Some(Value::Object(merge_into_object)), Value::Object(merge_from_object)) => {
                merge_event_maps(merge_into_object, merge_from_object);
            }
            (Some(Value::Array(merge_into_array)), Value::Array(merge_from_array))
                if key == "tags" =>
            {
                for item in merge_from_array {
                    if !merge_into_array.iter().any(|existing| existing == item) {
                        merge_into_array.push(item.clone());
                    }
                }
            }
            _ => {
                merge_into.insert(key.clone(), merge_from_value.clone());
            }
        }
    }
}

fn parse_numeric_map(value: Value) -> Option<HashMap<String, f64>> {
    match value {
        Value::Object(map) => Some(HashMap::from_iter(
            map.into_iter()
                .filter_map(|(key, value)| value.as_f64().map(|number| (key, number))),
        )),
        _ => None,
    }
}

fn parse_string_array(value: Value) -> Option<Vec<String>> {
    match value {
        Value::Array(values) => Some(
            values
                .into_iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanLog;
    use serde_json::{json, Value};
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

    #[test]
    fn apply_propagated_event_merges_fields_and_extra() {
        let propagated_event = Map::from_iter([
            ("metrics".to_string(), json!({ "foo": 0.1 })),
            (
                "span_attributes".to_string(),
                json!({ "purpose": "scorer", "source": "propagated" }),
            ),
            (
                "_async_scoring_control".to_string(),
                json!({ "kind": "state_override", "state": { "status": "disabled" } }),
            ),
        ]);

        let mut event_fields = MutableSpanEvent {
            metrics: Some(HashMap::from_iter([("start".to_string(), 1.0)])),
            span_attributes: Some(crate::types::SpanAttributes {
                name: Some("child".to_string()),
                span_type: Some(crate::types::SpanType::Facet),
                purpose: None,
                extra: HashMap::new(),
            }),
            ..Default::default()
        };

        apply_propagated_event(&propagated_event, &mut event_fields);

        assert_eq!(
            event_fields
                .metrics
                .as_ref()
                .and_then(|m| m.get("start").copied()),
            Some(1.0)
        );
        assert_eq!(
            event_fields
                .metrics
                .as_ref()
                .and_then(|m| m.get("foo").copied()),
            Some(0.1)
        );
        assert_eq!(
            event_fields
                .span_attributes
                .as_ref()
                .and_then(|attrs| attrs.purpose.as_deref()),
            Some("scorer")
        );
        assert_eq!(
            event_fields
                .span_attributes
                .as_ref()
                .and_then(|attrs| attrs.extra.get("source")),
            Some(&json!("propagated"))
        );
        assert!(event_fields.extra.contains_key("_async_scoring_control"));
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

        // Verify the logs request was made with the correct org_name
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
            rows[0].get("org_name").and_then(|v| v.as_str()),
            Some("Test Org")
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
}
