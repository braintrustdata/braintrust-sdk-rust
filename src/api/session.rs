use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use tokio::sync::Notify;
use url::Url;

use crate::api::config::ApiHttpConfig;
use crate::api::transport::build_http_client;
use crate::api::ApiClient;
use crate::error::{BraintrustError, Result};

/// Organization info returned from login.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgInfo {
    id: String,
    name: String,
    #[serde(default)]
    api_url: Option<String>,
}

impl OrgInfo {
    /// The organization's unique ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The organization's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The organization's API URL, if set.
    pub fn api_url(&self) -> Option<&str> {
        self.api_url.as_deref()
    }
}

/// Response from the login endpoint.
#[derive(Debug, Deserialize)]
struct LoginResponse {
    org_info: Vec<OrgInfo>,
}

/// Logged-in state containing API key and org info.
///
/// This type owns login readiness, waiter notification, the SDK HTTP client,
/// and construction of authenticated [`ApiClient`] instances.
#[derive(Clone)]
pub struct LoginState {
    shared: Arc<LoginStateShared>,
}

struct LoginStateShared {
    inner: std::sync::OnceLock<LoginStateInner>,
    notify: Notify,
    http_client: Client,
    login_timeout: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct LoginStateInner {
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
    /// Create a new empty login state with default API HTTP configuration.
    pub fn new() -> Self {
        Self::with_http_config(ApiHttpConfig::default())
            .expect("default API HTTP configuration should be valid")
    }

    pub(crate) fn with_http_config(config: ApiHttpConfig) -> Result<Self> {
        let http_client = build_http_client(config.request_timeout, config.ca_bundle.as_deref())?;
        Ok(Self {
            shared: Arc::new(LoginStateShared {
                inner: std::sync::OnceLock::new(),
                notify: Notify::new(),
                http_client,
                login_timeout: config.login_timeout,
            }),
        })
    }

    /// Set the login state (can only be called once).
    ///
    /// Returns true if set successfully, false if already set. Successful calls
    /// wake any tasks waiting for login completion.
    pub fn set(
        &self,
        api_key: String,
        org_id: String,
        org_name: String,
        api_url: String,
        app_url: String,
    ) -> bool {
        let did_set = self
            .shared
            .inner
            .set(LoginStateInner {
                api_key,
                org_id,
                org_name,
                api_url,
                app_url,
            })
            .is_ok();
        if did_set {
            self.shared.notify.notify_waiters();
        }
        did_set
    }

    /// Check if logged in.
    pub fn is_logged_in(&self) -> bool {
        self.shared.inner.get().is_some()
    }

    /// Wait for login state to be available.
    pub async fn wait(&self) -> Result<Self> {
        if self.is_logged_in() {
            return Ok(self.clone());
        }

        let notified = self.shared.notify.notified();

        if self.is_logged_in() {
            return Ok(self.clone());
        }

        tokio::select! {
            _ = notified => {
                if self.is_logged_in() {
                    Ok(self.clone())
                } else {
                    Err(BraintrustError::InvalidConfig(
                        "Login notification received but state not set".into(),
                    ))
                }
            }
            _ = tokio::time::sleep(self.shared.login_timeout) => {
                Err(BraintrustError::InvalidConfig(
                    "Timeout waiting for login to complete".into(),
                ))
            }
        }
    }

    pub async fn api_client(&self) -> Result<ApiClient> {
        self.wait().await?;
        self.api_client_now()
    }

    pub fn api_client_now(&self) -> Result<ApiClient> {
        ApiClient::from_login_state(self.clone())
    }

    /// Perform login synchronously and populate this session state.
    pub async fn login(
        &self,
        app_url: &str,
        fallback_api_url: &str,
        api_key: &str,
        org_name: Option<&str>,
    ) -> Result<()> {
        let login_url = Url::parse(app_url)
            .map_err(|err| BraintrustError::InvalidConfig(format!("invalid app_url: {err}")))?
            .join("api/apikey/login")
            .map_err(|err| BraintrustError::InvalidConfig(format!("invalid login URL: {err}")))?;

        let response = self
            .shared
            .http_client
            .post(login_url)
            .bearer_auth(api_key)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(BraintrustError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let login_response: LoginResponse =
            response.json().await.map_err(|err| BraintrustError::Api {
                status: 200,
                message: format!("Failed to parse login response: {err}"),
            })?;

        let org = if let Some(name) = org_name {
            login_response
                .org_info
                .into_iter()
                .find(|org| org.name == name)
                .ok_or_else(|| {
                    BraintrustError::InvalidConfig(format!("Organization '{name}' not found"))
                })?
        } else {
            login_response.org_info.into_iter().next().ok_or_else(|| {
                BraintrustError::InvalidConfig(
                    "No organizations found for this API key".to_string(),
                )
            })?
        };

        let did_set = self.set(
            api_key.to_string(),
            org.id,
            org.name,
            org.api_url.unwrap_or_else(|| fallback_api_url.to_string()),
            app_url.to_string(),
        );
        if !did_set {
            return Err(BraintrustError::InvalidConfig(
                "Login state already set".to_string(),
            ));
        }

        Ok(())
    }

    /// Start login in a background task with retry logic.
    pub fn start_background_login(
        &self,
        app_url: String,
        fallback_api_url: String,
        api_key: String,
        org_name: Option<String>,
    ) {
        let state = self.clone();
        tokio::spawn(async move {
            const MAX_ATTEMPTS: u32 = 10;
            let mut delay = Duration::from_millis(100);
            let max_delay = Duration::from_secs(5);

            for attempt in 1..=MAX_ATTEMPTS {
                match state
                    .login(&app_url, &fallback_api_url, &api_key, org_name.as_deref())
                    .await
                {
                    Ok(()) => {
                        tracing::debug!("Background login completed successfully");
                        return;
                    }
                    Err(err) if attempt < MAX_ATTEMPTS => {
                        tracing::warn!(
                            attempt,
                            max = MAX_ATTEMPTS,
                            "Background login failed: {}, retrying in {:?}",
                            err,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(max_delay);
                    }
                    Err(err) => {
                        tracing::error!(
                            "Background login failed after {} attempts: {}. \
                             Spans will not be uploaded until login state is populated.",
                            MAX_ATTEMPTS,
                            err
                        );
                    }
                }
            }
        });
    }

    pub(crate) fn http_client(&self) -> &Client {
        &self.shared.http_client
    }

    pub(crate) fn state(&self) -> Option<&LoginStateInner> {
        self.shared.inner.get()
    }

    /// Get the API key if logged in.
    pub fn api_key(&self) -> Option<String> {
        self.state().map(|state| state.api_key.clone())
    }

    /// Get the org ID if logged in.
    pub fn org_id(&self) -> Option<String> {
        self.state().map(|state| state.org_id.clone())
    }

    /// Get the org name if logged in.
    pub fn org_name(&self) -> Option<String> {
        self.state().map(|state| state.org_name.clone())
    }

    /// Get the API URL if logged in.
    pub fn api_url(&self) -> Option<String> {
        self.state().map(|state| state.api_url.clone())
    }

    /// Get the app URL if logged in.
    pub fn app_url(&self) -> Option<String> {
        self.state().map(|state| state.app_url.clone())
    }
}

impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.state() {
            Some(inner) => f.debug_struct("LoginState").field("inner", inner).finish(),
            None => f
                .debug_struct("LoginState")
                .field("inner", &"<not logged in>")
                .finish(),
        }
    }
}
