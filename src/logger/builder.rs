use super::{BraintrustClient, ClientInner, LoginState, DEFAULT_API_URL, DEFAULT_APP_URL};
use crate::error::{BraintrustError, Result};
use crate::http::{build_http_client, CaBundleConfig};
use crate::log_queue::{LogQueue, LogQueueConfig};
use crate::span::{SpanOrigin, SpanOriginEnvironment};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use url::Url;

// Covers both quick API calls and slower log uploads.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_QUEUE_SIZE: usize = 256;

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
    queue_size: usize,
    blocking_login: bool,
    skip_login: bool,
    /// Maximum items per HTTP batch.
    batch_max_items: Option<usize>,
    /// Maximum bytes per HTTP batch.
    batch_max_bytes: Option<usize>,
    /// Maximum queue capacity (None = unlimited).
    queue_max_size: Option<usize>,
    /// Whether the queue should enforce `queue_max_size`.
    enforce_queue_size_limit: bool,
    checked_delivery: bool,
    environment: Option<SpanOriginEnvironment>,
    span_origin: Option<SpanOrigin>,
    user_agent: Option<String>,
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
            queue_size: DEFAULT_QUEUE_SIZE,
            blocking_login: false,
            skip_login: false,
            batch_max_items: None,
            batch_max_bytes: None,
            queue_max_size: None,
            enforce_queue_size_limit: true,
            checked_delivery: false,
            environment: None,
            span_origin: None,
            user_agent: None,
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

    /// Set the maximum queue capacity.
    /// When full, the newest arriving events are dropped.
    /// Default: 15,000 (or `BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE` env var).
    pub fn with_queue_max_size(mut self, max_size: usize) -> Self {
        self.queue_max_size = Some(max_size);
        self
    }

    /// Legacy compatibility setting. Delivery admission is always bounded by
    /// `with_queue_max_size`, including in-flight writes. This value is ignored.
    pub fn enforce_queue_size_limit(mut self, enforce: bool) -> Self {
        self.enforce_queue_size_limit = enforce;
        self
    }

    /// Legacy compatibility setting. Delivery is always automatic and flush
    /// reports failures in covered active work. This value is ignored. Await a
    /// write completion to observe that write's result independently of flush.
    pub fn checked_delivery(mut self, checked: bool) -> Self {
        self.checked_delivery = checked;
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

    /// Build the client, performing login.
    ///
    /// If `blocking_login` is true, waits for login to complete.
    /// Otherwise, login happens in the background with retry logic.
    #[must_use = "await or call blocking_wait to obtain the result"]
    pub fn build(mut self) -> crate::Completion<BraintrustClient, BraintrustError> {
        crate::operation::submit(async move {
            let login = (!self.skip_login).then(|| {
                (
                    self.api_key.take(),
                    self.org_name.take(),
                    self.blocking_login,
                )
            });
            let client = self.construct()?;
            if let Some((api_key, org_name, blocking_login)) = login {
                let api_key = api_key.ok_or_else(|| {
                    BraintrustError::InvalidConfig(
                        "API key required: set BRAINTRUST_API_KEY or call .api_key()".into(),
                    )
                })?;
                if blocking_login {
                    client.perform_login(&api_key, org_name.as_deref()).await?;
                } else {
                    client.start_background_login(api_key, org_name);
                }
            }
            Ok(client)
        })
    }

    /// Build the client without logging in.
    ///
    /// Equivalent to `.skip_login(true).build()`, but synchronous, since only login
    /// needs to wait. Upload with per-call credentials, such as
    /// [`BraintrustClient::log_rows_with_credentials`] or
    /// [`BraintrustClient::log_rows_with_token_provider`]. The SDK selects its
    /// background runtime on first use.
    pub fn build_without_login(mut self) -> Result<BraintrustClient> {
        self.skip_login = true;
        self.construct()
    }

    fn construct(self) -> Result<BraintrustClient> {
        let app_url_str = self.app_url.unwrap_or_else(|| DEFAULT_APP_URL.into());
        let explicit_api_url = self.api_url.is_some();
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
            .enforce_queue_size_limit(self.enforce_queue_size_limit)
            .checked_delivery(self.checked_delivery)
            .build();

        // LogQueue owns the background worker. It is given the client-configured
        // URLs so that flushes with per-request credentials work without relying on
        // the shared (set-once) login state to hold URLs.
        let queue = LogQueue::new(
            crate::runtime::handle()?,
            log_config,
            login_state.clone(),
            http_client.clone(),
            self.queue_size,
            api_url.clone(),
            app_url.clone(),
        );

        let client = BraintrustClient {
            inner: Arc::new(ClientInner {
                api_url,
                explicit_api_url,
                app_url,
                queue,
                login_state,
                login_notify: Notify::new(),
                pending_auth: Arc::default(),
                http_client,
                default_project: self.default_project,
                login_skipped: self.skip_login,
                environment: self.environment,
                span_origin: self.span_origin,
            }),
        };

        Ok(client)
    }
}

impl Default for BraintrustClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_rejects_missing_api_key() {
        if crate::http::tests::run_isolated(
            "logger::builder::tests::builder_rejects_missing_api_key",
        ) {
            return;
        }
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
        if crate::http::tests::run_isolated(
            "logger::builder::tests::builder_rejects_malformed_environment_ca_bundle",
        ) {
            return;
        }
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
}
