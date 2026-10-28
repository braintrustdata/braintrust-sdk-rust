use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use url::Url;

use crate::dataset::DatasetBuilder;
use crate::error::{BraintrustError, Result};
use crate::experiments::ExperimentBuilder;
use crate::http::{build_http_client, CaBundleConfig};
use crate::internal::execution::ExecutionBinding;
use crate::internal::lifecycle::Lifecycle;
use crate::internal::ports::{PreparedSpan, SpanSubmitter};
use crate::internal::resources::ResourceTransport;
use crate::internal::retry::{exponential, RetryExecutor};
use crate::internal::session::CredentialScope;
use crate::log_queue::{LogQueue, LogQueueConfig};
use crate::span::{merge_span_origin_context, SpanOrigin, SpanOriginEnvironment};
use crate::span_components::SpanComponents;
use crate::types::{ParentSpanInfo, SpanAttributes, SpanEventData, SpanObjectType, SpanPayload};

// 30s covers both quick API calls (login, project registration) and slower batch log uploads.
// The TypeScript SDK applies no explicit timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
pub use crate::api::{DEFAULT_API_URL, DEFAULT_APP_URL};

use crate::api::login::LoginResponse;
pub use crate::api::login::OrgInfo;

/// Logged-in state containing API key and org info.
/// Handles internal synchronization for the transition from not-logged-in to logged-in.
#[derive(Clone)]
pub struct LoginState {
    inner: Arc<std::sync::OnceLock<LoginStateInner>>,
}

#[derive(Clone)]
struct LoginStateInner {
    api_key: String,
    org_id: String,
    org_name: String,
    api_url: String,
    app_url: String,
}

impl Default for LoginState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginState {
    /// Create a new empty login state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Set the login state (can only be called once).
    /// Returns true if set successfully, false if already set.
    pub fn set(
        &self,
        api_key: String,
        org_id: String,
        org_name: String,
        api_url: String,
        app_url: String,
    ) -> bool {
        self.inner
            .set(LoginStateInner {
                api_key,
                org_id,
                org_name,
                api_url,
                app_url,
            })
            .is_ok()
    }

    /// Capture the selected organization and routes together, without fallback.
    fn scope(&self) -> Result<CredentialScope> {
        let state = self
            .inner
            .get()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
        let api_url = Url::parse(&state.api_url)
            .map_err(|_| BraintrustError::InvalidConfig("invalid login API URL".into()))?;
        let app_url = Url::parse(&state.app_url)
            .map_err(|_| BraintrustError::InvalidConfig("invalid login app URL".into()))?;
        Ok(CredentialScope::new(
            state.api_key.clone(),
            state.org_id.clone(),
            Some(state.org_name.clone()),
            api_url,
            app_url,
        ))
    }

    /// Check if logged in.
    pub fn is_logged_in(&self) -> bool {
        self.inner.get().is_some()
    }

    /// Get the API key if logged in.
    pub fn api_key(&self) -> Option<String> {
        self.inner.get().map(|s| s.api_key.clone())
    }

    /// Get the org ID if logged in.
    pub fn org_id(&self) -> Option<String> {
        self.inner.get().map(|s| s.org_id.clone())
    }

    /// Get the org name if logged in.
    pub fn org_name(&self) -> Option<String> {
        self.inner.get().map(|s| s.org_name.clone())
    }

    /// Get the API URL if logged in.
    pub fn api_url(&self) -> Option<String> {
        self.inner.get().map(|s| s.api_url.clone())
    }

    /// Get the app URL if logged in.
    pub fn app_url(&self) -> Option<String> {
        self.inner.get().map(|s| s.app_url.clone())
    }
}

impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginState")
            .field("is_logged_in", &self.is_logged_in())
            .field("org_id", &self.org_id())
            .field("org_name", &self.org_name())
            .finish_non_exhaustive()
    }
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
    ca_bundle: Option<CaBundleConfig>,
    blocking_login: bool,
    skip_login: bool,
    /// Maximum items per HTTP batch.
    batch_max_items: Option<usize>,
    /// Maximum bytes per HTTP batch.
    batch_max_bytes: Option<usize>,
    /// Maximum accepted writes, including in-flight work (None uses environment defaults).
    queue_max_size: Option<usize>,
    environment: Option<SpanOriginEnvironment>,
    span_origin: Option<SpanOrigin>,
    user_agent: Option<String>,
    runtime: Option<tokio::runtime::Handle>,
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
    /// - `BRAINTRUST_CUSTOM_CA_BUNDLE`: Additional PEM certificates to trust
    pub fn new() -> Self {
        Self {
            api_key: std::env::var("BRAINTRUST_API_KEY").ok(),
            app_url: std::env::var("BRAINTRUST_APP_URL").ok(),
            api_url: std::env::var("BRAINTRUST_API_URL").ok(),
            org_name: std::env::var("BRAINTRUST_ORG_NAME").ok(),
            default_project: std::env::var("BRAINTRUST_DEFAULT_PROJECT").ok(),
            ca_bundle: CaBundleConfig::from_environment(),
            blocking_login: false,
            skip_login: false,
            batch_max_items: None,
            batch_max_bytes: None,
            queue_max_size: None,
            environment: None,
            span_origin: None,
            user_agent: None,
            runtime: None,
        }
    }

    /// Set the API key (overrides `BRAINTRUST_API_KEY` env var).
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set the HTTP User-Agent sent by this client's requests.
    ///
    /// Defaults to the Rust SDK crate name and version. Applications embedding
    /// the SDK can set their own identity, for example `bt/0.20.0`.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
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

    /// Set the maximum accepted writes, including in-flight work.
    /// When full, new writes return `EnqueueError::Full` at issuance.
    /// Default: 15,000 (or `BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE` env var).
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

    /// Set a path to a custom PEM CA bundle used for HTTPS requests made by the SDK client.
    /// This overrides `BRAINTRUST_CUSTOM_CA_BUNDLE`.
    pub fn ca_bundle(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.ca_bundle = Some(CaBundleConfig::File(path.into()));
        self
    }

    /// Set additional PEM certificates to trust for HTTPS requests made by the SDK client.
    /// This overrides `BRAINTRUST_CUSTOM_CA_BUNDLE`.
    pub fn custom_ca_bundle_pem(mut self, pem: impl AsRef<[u8]>) -> Self {
        self.ca_bundle = Some(CaBundleConfig::from_pem(pem, "custom CA bundle"));
        self
    }

    /// Wait for login before initialization completes (default: false).
    ///
    /// When `false`, the build completion returns a client while login continues
    /// in the background. When `true`, that completion also observes login.
    pub fn blocking_login(mut self, blocking: bool) -> Self {
        self.blocking_login = blocking;
        self
    }

    /// Skip login entirely (default: false).
    ///
    /// When `true`, the API key is not required and no login request is made.
    /// Use this when the client is only needed for submitting spans with
    /// explicit credentials via [`BraintrustClient::span_builder_with_credentials`].
    pub fn skip_login(mut self, skip: bool) -> Self {
        self.skip_login = skip;
        self
    }

    /// Set span-origin environment provenance for spans created by this client.
    pub fn environment(
        mut self,
        environment_type: impl Into<String>,
        name: Option<impl Into<String>>,
    ) -> Self {
        self.environment = Some(SpanOriginEnvironment::new(environment_type, name));
        self
    }

    /// Set a default `context.span_origin` override for all spans created by this client.
    ///
    /// Per-builder ([`SpanBuilder::span_origin`](crate::SpanBuilder)) and per-span
    /// ([`SpanLog::span_origin`](crate::SpanLog)) overrides take precedence.
    pub fn span_origin(mut self, origin: SpanOrigin) -> Self {
        self.span_origin = Some(origin);
        self
    }

    /// Select one runtime for initialization, resource I/O and delivery.
    /// `None` (the default) owns and drives an internal runtime. An explicit
    /// handle requires its caller-owned runtime to stay alive and progressing.
    pub fn runtime(mut self, runtime: Option<tokio::runtime::Handle>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Schedule client initialization immediately and observe its result.
    /// Dropping the completion does not cancel initialization or login.
    pub fn build(self) -> crate::Completion<BraintrustClient, BraintrustError> {
        let validate = || -> Result<()> {
            Url::parse(self.app_url.as_deref().unwrap_or(DEFAULT_APP_URL))
                .map_err(|_| BraintrustError::InvalidConfig("invalid app_url".into()))?;
            Url::parse(self.api_url.as_deref().unwrap_or(DEFAULT_API_URL))
                .map_err(|_| BraintrustError::InvalidConfig("invalid api_url".into()))?;
            if !self.skip_login && self.api_key.is_none() {
                return Err(BraintrustError::InvalidConfig(
                    "API key required: set BRAINTRUST_API_KEY or call .api_key()".into(),
                ));
            }
            Ok(())
        };
        let binding = validate().and_then(|()| ExecutionBinding::new(self.runtime.clone()));
        match binding {
            Ok(binding) => {
                let context = binding.clone();
                binding.response(async move { self.initialize(context).await })
            }
            Err(error) => crate::Completion::rejected(error),
        }
    }

    async fn initialize(self, execution: ExecutionBinding) -> Result<BraintrustClient> {
        let app_url_str = self.app_url.unwrap_or_else(|| DEFAULT_APP_URL.into());
        let api_url_str = self.api_url.unwrap_or_else(|| DEFAULT_API_URL.into());

        let app_url = Url::parse(&app_url_str)
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid app_url: {}", e)))?;
        let api_url = Url::parse(&api_url_str)
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid api_url: {}", e)))?;

        let http_client = build_http_client(
            REQUEST_TIMEOUT,
            self.ca_bundle.as_ref(),
            self.user_agent.as_deref(),
        )?;

        // Create login state (initially empty, populated by login)
        let login_state = LoginState::new();

        // Build log queue config
        let log_config = LogQueueConfig::builder()
            .maybe_batch_max_items(self.batch_max_items)
            .maybe_batch_max_bytes(self.batch_max_bytes)
            .maybe_queue_max_size(self.queue_max_size)
            .build();

        // Commands carry their own immutable scope; the worker has no session fallback.
        let queue = LogQueue::new(log_config, http_client.clone(), execution.clone());

        let client = BraintrustClient {
            inner: Arc::new(ClientInner {
                api_url,
                app_url,
                lifecycle: Lifecycle::new(execution.clone(), queue.clone()),
                execution,
                queue,
                login_state,
                login_notify: Notify::new(),
                http_client,
                default_project: self.default_project,
                login_skipped: self.skip_login,
                environment: self.environment,
                span_origin: self.span_origin,
            }),
        };

        // Perform login
        if !self.skip_login {
            let api_key = self.api_key.ok_or_else(|| {
                BraintrustError::InvalidConfig(
                    "API key required: set BRAINTRUST_API_KEY or call .api_key()".into(),
                )
            })?;

            if self.blocking_login {
                client
                    .perform_login(&api_key, self.org_name.as_deref())
                    .await?;
            } else {
                client.start_background_login(api_key, self.org_name);
            }
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

/// Validated, owned patch data without unresolved or placeholder credentials.
struct SpanUpdate {
    row_id: String,
    span_id: String,
    components: SpanComponents,
    event_data: SpanEventData,
}
impl SpanUpdate {
    fn into_prepared(self, scope: CredentialScope) -> PreparedSpan {
        let Self {
            row_id,
            span_id,
            components,
            event_data,
        } = self;
        let payload = SpanPayload {
            row_id,
            span_id,
            is_merge: true,
            span_components: Some(components),
            org_id: scope.org_id().to_owned(),
            org_name: scope.org_name().map(str::to_owned),
            project_name: None,
            input: event_data.input,
            output: event_data.output,
            expected: event_data.expected,
            error: event_data.error,
            scores: event_data.scores,
            metadata: event_data.metadata,
            metrics: event_data.metrics,
            tags: event_data.tags,
            context: event_data.context,
            span_attributes: event_data.span_attributes,
            extra: event_data.extra,
        };

        PreparedSpan {
            scope,
            payload,
            parent_info: None,
        }
    }
}

struct ClientInner {
    lifecycle: Lifecycle,
    execution: ExecutionBinding,
    api_url: Url,
    app_url: Url,
    queue: LogQueue,
    login_state: LoginState,
    login_notify: Notify,
    http_client: reqwest::Client,
    default_project: Option<String>,
    login_skipped: bool,
    environment: Option<SpanOriginEnvironment>,
    span_origin: Option<SpanOrigin>,
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
    fn resource_transport(&self) -> Arc<ResourceTransport> {
        Arc::new(ResourceTransport::new(self.inner.http_client.clone()))
    }

    fn span_submitter(&self) -> Arc<dyn SpanSubmitter> {
        Arc::new(self.inner.queue.clone())
    }

    fn explicit_scope(
        &self,
        token: String,
        org_id: String,
        org_name: Option<String>,
    ) -> CredentialScope {
        CredentialScope::new(
            token,
            org_id,
            org_name,
            self.inner.api_url.clone(),
            self.inner.app_url.clone(),
        )
    }

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

    /// Check local login state immediately, without scheduling work.
    pub fn is_logged_in(&self) -> bool {
        self.inner.login_state.is_logged_in()
    }

    /// Wait for login to complete (useful if using background login).
    ///
    /// Submits the wait immediately on the selected runtime. Awaiting observes
    /// the login state or a timeout; dropping the token does not cancel the wait.
    /// Login-dependent methods reject issuance when login was skipped.
    pub fn wait_for_login(&self) -> crate::Completion<LoginState, BraintrustError> {
        if self.inner.login_skipped {
            return crate::Completion::rejected(BraintrustError::InvalidConfig(
                "wait_for_login() not available when skip_login is enabled".into(),
            ));
        }
        let client = self.clone();
        self.inner
            .execution
            .response(async move { client.wait_for_login_state().await })
    }

    /// Get a shared view of local login state without scheduling work.
    pub fn login_state(&self) -> LoginState {
        self.inner.login_state.clone()
    }

    /// Create a span builder using the logged-in state and default project.
    ///
    /// Submits immediately, waiting for login on the selected runtime if needed.
    /// Dropping the returned completion does not cancel accepted work.
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
    pub fn span_builder(&self) -> crate::Completion<crate::span::SpanBuilder, BraintrustError> {
        if self.inner.login_skipped {
            return crate::Completion::rejected(BraintrustError::InvalidConfig(
                "span_builder() requires login; use span_builder_with_credentials() when skip_login is enabled".into(),
            ));
        }
        let client = self.clone();
        self.inner
            .execution
            .response(async move { client.span_builder_inner().await })
    }

    async fn span_builder_inner(&self) -> Result<crate::span::SpanBuilder> {
        let state = self.wait_for_login_state().await?;
        let mut builder = crate::span::SpanBuilder::new(self.span_submitter(), state.scope()?);
        if let Some(environment) = &self.inner.environment {
            builder = builder.span_origin_environment(environment.clone());
        }
        if let Some(span_origin) = &self.inner.span_origin {
            builder = builder.span_origin(span_origin.clone());
        }
        if let Some(ref project) = self.inner.default_project {
            builder = builder.project_name(project);
        }
        Ok(builder)
    }

    /// Create a dataset builder using the logged-in state.
    ///
    /// Submits immediately, waiting for login on the selected runtime if needed.
    /// Dropping the returned completion does not cancel accepted work.
    pub fn dataset_builder(&self) -> crate::Completion<DatasetBuilder, BraintrustError> {
        if self.inner.login_skipped {
            return crate::Completion::rejected(BraintrustError::InvalidConfig("dataset_builder() requires login; use dataset_builder_with_credentials() when skip_login is enabled".into()));
        }
        let client = self.clone();
        self.inner
            .execution
            .response(async move { client.dataset_builder_inner().await })
    }

    async fn dataset_builder_inner(&self) -> Result<DatasetBuilder> {
        let state = self.wait_for_login_state().await?;
        Ok(DatasetBuilder::new(
            self.resource_transport(),
            self.inner.execution.clone(),
            Arc::new(self.inner.queue.clone()),
            state.scope()?,
        ))
    }

    /// Create a dataset builder with immutable explicit credentials, without
    /// logging in or changing the client's shared session.
    pub fn dataset_builder_with_credentials(
        &self,
        token: impl Into<String>,
        org_name: impl Into<String>,
    ) -> DatasetBuilder {
        DatasetBuilder::new(
            self.resource_transport(),
            self.inner.execution.clone(),
            Arc::new(self.inner.queue.clone()),
            self.explicit_scope(token.into(), String::new(), Some(org_name.into())),
        )
    }

    /// Create a span builder with explicit token and org_id.
    ///
    /// Use this for per-session / multi-tenant logging: the supplied credentials
    /// and configured endpoint routes are captured by the builder and travel with
    /// each span. They do not change the client's shared login state. Spans created from the same client with
    /// different `(token, org_id)` pairs each upload with their own credentials.
    pub fn span_builder_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> crate::span::SpanBuilder {
        let token = token.into();
        let org_id = org_id.into();

        let submitter = self.span_submitter();
        let mut builder =
            crate::span::SpanBuilder::new(submitter, self.explicit_scope(token, org_id, None));
        if let Some(environment) = &self.inner.environment {
            builder = builder.span_origin_environment(environment.clone());
        }
        if let Some(span_origin) = &self.inner.span_origin {
            builder = builder.span_origin(span_origin.clone());
        }
        builder
    }

    /// Submit an update using exported span context and the shared login state.
    /// Validation and admission happen immediately. The completion includes
    /// login (if pending), preparation and full batch delivery.
    pub fn update_span(
        &self,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> crate::Completion<(), BraintrustError> {
        let scope = if self.inner.login_state.is_logged_in() {
            match self.inner.login_state.scope() {
                Ok(scope) => Some(scope),
                Err(error) => return crate::Completion::rejected(error),
            }
        } else if self.inner.login_skipped {
            return crate::Completion::rejected(BraintrustError::InvalidConfig(
                "update_span() requires a logged-in client; use update_span_with_credentials() when skip_login is enabled".into(),
            ));
        } else {
            None
        };
        self.schedule_span_update(scope, exported, event)
    }

    /// Submit an update with immutable explicit credentials and configured routes.
    /// Credentials travel with this write and never populate shared login state.
    /// Dropping the token does not cancel an admitted update.
    pub fn update_span_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> crate::Completion<(), BraintrustError> {
        let scope = self.explicit_scope(token.into(), org_id.into(), None);
        self.schedule_span_update(Some(scope), exported, event)
    }

    // None means the immutable session scope is still waiting for login. The
    // queue owns that resolution before any flush or shutdown can capture it.
    fn schedule_span_update(
        &self,
        scope: Option<CredentialScope>,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> crate::Completion<(), BraintrustError> {
        self.inner.execution.response_try(|| {
            let update = self.prepare_span_update(exported, event)?;
            let delivery = match scope {
                Some(scope) => {
                    let PreparedSpan {
                        scope,
                        payload,
                        parent_info,
                    } = update.into_prepared(scope);
                    self.submit_payload(scope, payload, parent_info)
                }
                None => {
                    let client = self.clone();
                    self.inner.queue.submit_deferred(
                        update.row_id.clone(),
                        Box::pin(async move {
                            let state = client.wait_for_login_state().await?;
                            Ok(update.into_prepared(state.scope()?))
                        }),
                    )
                }
            };
            if let Err(error) = &*delivery {
                return Err(error.clone().into());
            }
            Ok(async move {
                delivery.await?;
                Ok(())
            })
        })
    }

    fn prepare_span_update(
        &self,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> Result<SpanUpdate> {
        let components = SpanComponents::parse(exported)?;
        let row_id = components.row_id.clone().ok_or_else(|| {
            BraintrustError::InvalidConfig("Exported span must have a row_id".into())
        })?;

        if components.root_span_id.is_some() != components.span_id.is_some() {
            return Err(BraintrustError::InvalidConfig(
                "both root_span_id and span_id must be set, or neither".into(),
            ));
        }

        if components.object_id.is_none() {
            match components.object_type {
                SpanObjectType::ProjectLogs => {
                    if components.project_logs_identifier().is_none() {
                        return Err(BraintrustError::InvalidConfig(
                            "project-log compute_object_metadata_args must include project_id or project_name".into(),
                        ));
                    }
                }
                SpanObjectType::Experiment => {
                    return Err(BraintrustError::InvalidConfig(
                        "Exported experiment span must include object_id".into(),
                    ));
                }
                SpanObjectType::PlaygroundLogs => {
                    return Err(BraintrustError::InvalidConfig(
                        "Exported playground span must include object_id".into(),
                    ));
                }
            }
        }

        let span_id = components.span_id.clone().unwrap_or_else(|| row_id.clone());
        let mut event_data = SpanEventData {
            input: event.input,
            output: event.output,
            expected: event.expected,
            error: event.error,
            scores: event.scores,
            metadata: event.metadata,
            metrics: event.metrics,
            tags: event.tags,
            context: merge_span_origin_context(
                event.context,
                self.inner.environment.as_ref(),
                event
                    .span_origin
                    .as_ref()
                    .or(self.inner.span_origin.as_ref()),
                event.skip_span_origin,
            ),
            span_attributes: {
                let extra = event.span_attributes_extra.unwrap_or_default();
                if event.name.is_some() || !extra.is_empty() {
                    Some(SpanAttributes {
                        name: event.name,
                        span_type: None,
                        purpose: None,
                        extra,
                    })
                } else {
                    None
                }
            },
            extra: HashMap::new(),
        };
        if let Some(propagated_event) = components.propagated_event.as_ref() {
            event_data.apply_propagated_event(propagated_event);
        }

        Ok(SpanUpdate {
            row_id,
            span_id,
            components,
            event_data,
        })
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

        let did_set = self.inner.login_state.set(
            api_key.to_string(),
            org.id,
            org.name,
            org.api_url
                .unwrap_or_else(|| self.inner.api_url.to_string()),
            self.inner.app_url.to_string(),
        );
        if !did_set {
            return Err(BraintrustError::InvalidConfig(
                "Login state already set".to_string(),
            ));
        }

        self.inner.login_notify.notify_waiters();
        Ok(())
    }

    /// Start login in a background task with retry logic.
    fn start_background_login(&self, api_key: String, org_name: Option<String>) {
        let client = self.clone();
        self.inner.execution.response(async move {
            const MAX_ATTEMPTS: usize = 10;
            let outcome = RetryExecutor::new(
                MAX_ATTEMPTS - 1,
                exponential(Duration::from_millis(100), Duration::from_secs(5)),
            )
            .run(
                |_| client.perform_login(&api_key, org_name.as_deref()),
                |_| true, // Preserve the existing background-login policy.
                |attempt, error, delay| {
                    tracing::warn!(
                        attempt = attempt.index + 1,
                        max = MAX_ATTEMPTS,
                        "Background login failed: {}, retrying in {:?}",
                        error,
                        delay
                    );
                },
            )
            .await;
            match &outcome.result {
                Ok(()) => tracing::debug!("Background login completed successfully"),
                Err(error) => tracing::error!(
                    "Background login failed after {} attempts: {}. \
                     Spans will not be uploaded until the client is re-created.",
                    outcome.attempts,
                    error
                ),
            }
            outcome.result
        });
    }

    /// Wait for login state to be available.
    async fn wait_for_login_state(&self) -> Result<LoginState> {
        // Check if already logged in
        if self.inner.login_state.is_logged_in() {
            return Ok(self.inner.login_state.clone());
        }

        // Get notification future BEFORE checking state to avoid race condition
        let notified = self.inner.login_notify.notified();

        // Check state again (may have been set between our first check and now)
        if self.inner.login_state.is_logged_in() {
            return Ok(self.inner.login_state.clone());
        }

        // Wait for notification or timeout
        tokio::select! {
            _ = notified => {
                // Login completed - return the login state
                if self.inner.login_state.is_logged_in() {
                    Ok(self.inner.login_state.clone())
                } else {
                    Err(BraintrustError::InvalidConfig(
                        "Login notification received but state not set".into(),
                    ))
                }
            }
            _ = tokio::time::sleep(LOGIN_TIMEOUT) => {
                Err(BraintrustError::InvalidConfig(
                    "Timeout waiting for login to complete".into(),
                ))
            }
        }
    }

    /// Create an experiment builder with explicit credentials.
    ///
    /// Credentials belong to this resource and never populate shared login state.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let experiment = client
    ///     .experiment_builder_with_credentials(&api_key, &org_id)
    ///     .project_name("my-project")
    ///     .experiment_name("baseline")
    ///     .build()?;
    /// ```
    pub fn experiment_builder_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> ExperimentBuilder {
        let token = token.into();
        let org_id = org_id.into();

        let submitter = self.span_submitter();
        ExperimentBuilder::new(
            self.resource_transport(),
            self.inner.execution.clone(),
            submitter,
            self.explicit_scope(token, org_id, None),
        )
    }

    /// Submit immediately; the token observes admission and full batch delivery.
    pub(crate) fn submit_payload(
        &self,
        scope: CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        self.inner.queue.submit(scope, payload, parent_info, None)
    }

    /// Stop response-task and write admission, drain accepted work, and stop delivery.
    /// Concurrent calls share the first call's deadline and outcome. Individual
    /// HTTP operation results stay on their own tokens; `delivered` counts writes.
    /// A timeout fails remaining completions and discards active SDK state.
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> crate::Completion<crate::ShutdownReport, crate::ShutdownError> {
        self.inner.lifecycle.shutdown(timeout)
    }

    /// Capture a cutoff immediately and observe delivery of the covered batch.
    /// New writes belong to later work; completed failures are never replayed.
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        self.inner.queue.flush()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn login_state_debug_never_exposes_tokens_or_unvalidated_endpoints() {
        let state = super::LoginState::new();
        state.set(
            "private-token".into(),
            "org".into(),
            "tenant".into(),
            "https://user:password@example.com/?api_key=secret".into(),
            "https://example.com/?token=secret".into(),
        );
        let debug = format!("{state:?}");
        assert!(debug.contains("tenant"));
        for secret in ["private-token", "password", "secret"] {
            assert!(!debug.contains(secret));
        }
    }

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
    async fn shutdown_covers_accepted_response_tasks_even_without_a_log_worker() {
        let client = BraintrustClient::builder()
            .skip_login(true)
            .build()
            .await
            .unwrap();
        let response = client
            .inner
            .execution
            .response::<(), BraintrustError, _>(std::future::pending());
        assert!(response.is_ok());
        let shutdown = client.shutdown(Duration::from_millis(10));
        assert!(matches!(
            shutdown.await,
            Err(crate::ShutdownError::Execution(
                crate::ExecutionError::TimedOut
            ))
        ));
        assert!(matches!(
            response.await,
            Err(BraintrustError::Execution(crate::ExecutionError::TimedOut))
        ));
        assert_eq!(client.inner.execution.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn concurrent_shutdowns_share_the_first_deadline_and_reject_both_admission_domains() {
        let client = BraintrustClient::builder()
            .skip_login(true)
            .build()
            .await
            .unwrap();
        let response = client
            .inner
            .execution
            .response::<(), BraintrustError, _>(std::future::pending());
        let first = client.shutdown(Duration::from_millis(10));
        let concurrent = client.clone().shutdown(Duration::from_secs(60));
        assert!(matches!(
            client
                .inner
                .execution
                .response::<(), BraintrustError, _>(async { Ok(()) })
                .await,
            Err(BraintrustError::Admission(crate::EnqueueError::Closed))
        ));
        let span = client
            .span_builder_with_credentials("token", "org")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "project".into(),
            })
            .build();
        assert!(matches!(
            span.end().await,
            Err(crate::WriteError::Admission(crate::EnqueueError::Closed))
        ));
        for token in [first, concurrent, client.shutdown(Duration::ZERO)] {
            assert!(matches!(
                token.await,
                Err(crate::ShutdownError::Execution(
                    crate::ExecutionError::TimedOut
                ))
            ));
        }
        assert!(matches!(
            response.await,
            Err(BraintrustError::Execution(crate::ExecutionError::TimedOut))
        ));
        assert_eq!(client.inner.execution.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn a_write_failure_does_not_short_circuit_draining_response_tasks() {
        use futures::FutureExt;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .skip_login(true)
            .runtime(crate::current_runtime())
            .api_url(server.uri())
            .app_url(server.uri())
            .build()
            .await
            .unwrap();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let work_gate = gate.clone();
        let response = client
            .inner
            .execution
            .response::<(), BraintrustError, _>(async move {
                work_gate.acquire().await.unwrap().forget();
                Ok(())
            });
        let span = client
            .span_builder_with_credentials("token", "org")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "project".into(),
            })
            .build();
        let write = span.end();
        let shutdown = client.shutdown(Duration::from_secs(5));
        assert!(write.await.is_err());
        let mut shutdown = Box::pin(std::future::IntoFuture::into_future(shutdown));
        assert!(shutdown.as_mut().now_or_never().is_none());
        gate.add_permits(1);
        response.await.unwrap();
        assert!(
            matches!(shutdown.await, Err(crate::ShutdownError::Delivery(failure)) if failure.failed == 1)
        );
        assert_eq!(client.inner.execution.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn shutdown_has_reserved_control_capacity_when_response_admission_is_full() {
        let client = BraintrustClient::builder()
            .skip_login(true)
            .runtime(crate::current_runtime())
            .build()
            .await
            .unwrap();
        let responses = (0..crate::internal::response_tasks::MAX_RESPONSE_TASKS)
            .map(|_| {
                let response = client
                    .inner
                    .execution
                    .response::<(), BraintrustError, _>(std::future::pending());
                assert!(response.is_ok());
                response
            })
            .collect::<Vec<_>>();
        let shutdown = client.shutdown(Duration::from_millis(10));
        assert!(shutdown.is_ok());
        assert!(matches!(
            shutdown.await,
            Err(crate::ShutdownError::Execution(
                crate::ExecutionError::TimedOut
            ))
        ));
        for response in responses {
            assert!(matches!(
                response.await,
                Err(BraintrustError::Execution(crate::ExecutionError::TimedOut))
            ));
        }
        assert_eq!(client.inner.execution.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn response_operation_errors_stay_on_their_tokens_instead_of_a_shutdown_failure_ledger() {
        let client = BraintrustClient::builder()
            .skip_login(true)
            .runtime(crate::current_runtime())
            .build()
            .await
            .unwrap();
        let response = client
            .inner
            .execution
            .response::<(), BraintrustError, _>(async {
                Err(BraintrustError::Api {
                    status: 503,
                    message: "operation failed".into(),
                })
            });
        let shutdown = client.shutdown(Duration::from_secs(1));
        assert!(matches!(
            response.await,
            Err(BraintrustError::Api { status: 503, .. })
        ));
        assert_eq!(shutdown.await.unwrap().delivered, 0);
        assert_eq!(client.inner.execution.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn reads_and_empty_lifecycle_calls_do_not_start_a_log_worker() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/api/dataset/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"project":{"id":"project","name":"p"},"dataset":{"id":"dataset","name":"d"}})))
            .expect(1).mount(&server).await;
        let client = BraintrustClient::builder()
            .skip_login(true)
            .app_url(server.uri())
            .api_url(server.uri())
            .build()
            .await
            .unwrap();
        assert!(!client.inner.queue.is_started());
        client.flush().await.unwrap();
        let dataset = client
            .dataset_builder_with_credentials("token", "tenant")
            .project_name("p")
            .dataset_name("d")
            .build()
            .unwrap();
        assert_eq!(dataset.dataset_id().await.unwrap(), "dataset");
        assert!(!client.inner.queue.is_started());
        client.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(!client.inner.queue.is_started());
        let span = client
            .span_builder_with_credentials("token", "org")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "project".into(),
            })
            .build();
        assert!(span.log(SpanLog::default()).is_err());
        assert!(!client.inner.queue.is_started());
    }

    #[tokio::test]
    async fn resource_ports_do_not_retain_the_whole_client_or_login_state() {
        let client = BraintrustClient::builder()
            .skip_login(true)
            .build()
            .await
            .unwrap();
        let weak = Arc::downgrade(&client.inner);
        let span = client.span_builder_with_credentials("token", "org");
        let dataset = client
            .dataset_builder_with_credentials("token", "tenant")
            .project_name("p")
            .dataset_name("d");
        let experiment = client
            .experiment_builder_with_credentials("token", "org")
            .project_name("p");
        drop(client);
        assert!(
            weak.upgrade().is_none(),
            "resource dependencies must not retain ClientInner"
        );
        let _span = span.build();
        let _dataset = dataset.build().unwrap();
        let _experiment = experiment.build().unwrap();
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
    async fn builder_accepts_custom_ca_bundle_when_login_is_skipped() {
        let path = crate::http::tests::write_temp_bundle(crate::http::tests::VALID_TEST_CERT_PEM);

        let client = BraintrustClient::builder()
            .app_url("https://example.com")
            .api_url("https://example.com")
            .ca_bundle(&path)
            .skip_login(true)
            .build()
            .await;

        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn builder_accepts_raw_custom_ca_bundle_when_login_is_skipped() {
        let client = BraintrustClient::builder()
            .app_url("https://example.com")
            .api_url("https://example.com")
            .custom_ca_bundle_pem(crate::http::tests::VALID_TEST_CERT_PEM)
            .skip_login(true)
            .build()
            .await;

        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn builder_rejects_malformed_environment_ca_bundle() {
        let _lock = crate::http::tests::ENV_LOCK.lock().await;
        let original = std::env::var_os(crate::http::CUSTOM_CA_BUNDLE_ENV);
        std::env::set_var(
            crate::http::CUSTOM_CA_BUNDLE_ENV,
            "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
        );

        let result = BraintrustClient::builder()
            .app_url("https://example.com")
            .api_url("https://example.com")
            .skip_login(true)
            .build()
            .await;

        match original {
            Some(value) => std::env::set_var(crate::http::CUSTOM_CA_BUNDLE_ENV, value),
            None => std::env::remove_var(crate::http::CUSTOM_CA_BUNDLE_ENV),
        }

        let error = result.expect_err("malformed environment CA bundle should fail");
        assert!(error
            .to_string()
            .contains(crate::http::CUSTOM_CA_BUNDLE_ENV));
    }

    #[tokio::test]
    async fn builder_capacity_is_always_bounded() {
        let client = BraintrustClient::builder()
            .runtime(crate::current_runtime())
            .app_url("https://example.com")
            .api_url("https://example.com")
            .skip_login(true)
            .with_queue_max_size(1)
            .build()
            .await
            .expect("client");

        let span = client.span_builder_with_credentials("token", "org").build();
        assert!(span.log(crate::SpanLog::default()).is_ok());
        assert!(matches!(
            &*span.log(crate::SpanLog::default()),
            Err(crate::WriteError::Admission(crate::EnqueueError::Full))
        ));
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
            );
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
        );
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

        let login_state = client.login_state();
        assert!(login_state.is_logged_in(), "should be logged in");
        assert_eq!(login_state.org_id().as_deref(), Some("org-1"));
        assert_eq!(login_state.org_name().as_deref(), Some("First Org"));
        assert_eq!(login_state.api_key().as_deref(), Some("test-api-key"));
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

        let login_state = client.login_state();
        assert!(login_state.is_logged_in(), "should be logged in");
        assert_eq!(login_state.org_id().as_deref(), Some("org-2"));
        assert_eq!(login_state.org_name().as_deref(), Some("Second Org"));
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
        let is_logged_in = client.is_logged_in();
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
        assert_eq!(login_state.org_id().as_deref(), Some("org-1"));
        assert!(client.is_logged_in());
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
        });
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
        });
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
    async fn per_session_credentials_do_not_leak_and_batch_separately() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        // skip_login: the client never logs in, so any credential leak into the
        // shared login state would be observable via is_logged_in().
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .build()
            .await
            .expect("client");

        // Two sessions with different (token, org_id). Use ProjectLogs parents so no
        // project registration HTTP call is needed.
        let span_a = client
            .span_builder_with_credentials("token-a", "org-a")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "proj-a".into(),
            })
            .build();
        let span_b = client
            .span_builder_with_credentials("token-b", "org-b")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "proj-b".into(),
            })
            .build();

        span_a.log(SpanLog {
            input: Some(Value::String("a".into())),
            ..Default::default()
        });
        span_b.log(SpanLog {
            input: Some(Value::String("b".into())),
            ..Default::default()
        });
        span_a.flush().await.expect("flush a");
        span_b.flush().await.expect("flush b");
        client.flush().await.expect("client flush");

        // No credential leak into shared global state.
        assert!(
            !client.is_logged_in(),
            "per-request credentials must not populate the shared login state"
        );

        // Each /logs3 request must carry its own bearer token and matching org_id,
        // and a single request must never mix the two orgs (batched by credentials).
        let requests = server.received_requests().await.unwrap();
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for request in requests.iter().filter(|r| r.url.path() == "/logs3") {
            let auth = request
                .headers
                .get(
                    &"authorization"
                        .parse::<wiremock::http::HeaderName>()
                        .unwrap(),
                )
                .map(|values| values.to_string())
                .expect("authorization header");
            let body: Value = serde_json::from_slice(&request.body).expect("json");
            let rows = body.get("rows").and_then(|r| r.as_array()).expect("rows");
            let orgs: std::collections::HashSet<&str> = rows
                .iter()
                .filter_map(|row| row.get("org_id").and_then(Value::as_str))
                .collect();
            assert_eq!(orgs.len(), 1, "one request must not mix orgs: {orgs:?}");
            seen.insert(auth, orgs.into_iter().next().unwrap().to_string());
        }

        // Each token uploaded exactly its own org's rows, and never the other's.
        let org_for = |token: &str| -> Option<&String> {
            seen.iter()
                .find(|(auth, _)| auth.contains(token))
                .map(|(_, org)| org)
        };
        assert_eq!(org_for("token-a").map(String::as_str), Some("org-a"));
        assert_eq!(org_for("token-b").map(String::as_str), Some("org-b"));
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

        // Mount version endpoint on api_server (data plane) - for lazy version fetch
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&api_server)
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
        });
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify api_server received the /logs3 request (and optionally /version for lazy fetch)
        let api_requests = api_server.received_requests().await.unwrap();
        let logs3_requests: Vec<_> = api_requests
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .collect();
        assert_eq!(logs3_requests.len(), 1);

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
