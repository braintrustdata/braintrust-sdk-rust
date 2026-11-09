use super::{
    DEFAULT_HTTP_TIMEOUT, HttpError, api_error, build_auth_transport,
    build_auth_transport_from_builder,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
struct OAuthErrorResponse {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SetupWizardSession {
    pub session_token: String,
    pub poll_token: String,
    pub login_path: String,
    #[serde(default, alias = "verificationCode")]
    pub verification_code: Option<String>,
    pub expires_at: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SetupWizardPollResult {
    Pending {
        #[serde(rename = "expires_at")]
        _expires_at: String,
    },
    Expired,
    Claimed,
    Complete {
        api_key: String,
        org_id: String,
        org_name: String,
        project_id: String,
        project_name: String,
    },
}
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OAuthFailure {
    #[error("oauth token request rejected: {error} ({description:?})")]
    Rejected {
        error: String,
        description: Option<String>,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
pub struct OAuthClient {
    endpoint: String,
    http: crate::Transport,
}
impl OAuthClient {
    pub fn new(api_url: &str) -> Result<Self> {
        let http = build_auth_transport_from_builder(
            reqwest::Client::builder()
                .timeout(DEFAULT_HTTP_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none()),
        )?;
        Ok(Self {
            endpoint: format!("{}/oauth/token", api_url.trim_end_matches('/')),
            http,
        })
    }
    pub async fn exchange_code(
        &self,
        client_id: &str,
        redirect_uri: &str,
        code: &str,
        verifier: &str,
    ) -> std::result::Result<OAuthTokenResponse, OAuthFailure> {
        self.request(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("code", code),
            ("code_verifier", verifier),
        ])
        .await
    }
    pub async fn refresh(
        &self,
        client_id: &str,
        refresh_token: &str,
    ) -> std::result::Result<OAuthTokenResponse, OAuthFailure> {
        self.request(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .await
    }
    async fn request(
        &self,
        params: &[(&str, &str)],
    ) -> std::result::Result<OAuthTokenResponse, OAuthFailure> {
        let response = self
            .http
            .post(&self.endpoint)
            .form(params)
            .send()
            .await
            .map_err(|error| {
                if let Some(api) = error.api()
                    && let Ok(error) = serde_json::from_str::<OAuthErrorResponse>(&api.body)
                {
                    return OAuthFailure::Rejected {
                        error: error.error.unwrap_or_else(|| "unknown".into()),
                        description: error.error_description,
                    };
                }
                OAuthFailure::Other(api_error(error).context("failed to call oauth token endpoint"))
            })?;
        response
            .json()
            .await
            .context("failed to parse oauth token response")
            .map_err(OAuthFailure::Other)
    }
}
pub struct BrowserSetupClient {
    app_url: url::Url,
    http: crate::Transport,
}
impl BrowserSetupClient {
    pub fn new(app_url: &str) -> Result<Self> {
        Ok(Self {
            app_url: url::Url::parse(app_url)?,
            http: build_auth_transport(DEFAULT_HTTP_TIMEOUT)?,
        })
    }
    pub async fn create_session(&self) -> Result<SetupWizardSession> {
        let response = self
            .http
            .post(self.app_url.join("/api/cli/wizard-session/create")?)
            .send()
            .await
            .map_err(api_error)?;
        Ok(response.json().await?)
    }
    pub async fn poll(
        &self,
        session_token: &str,
        poll_token: &str,
    ) -> Result<SetupWizardPollResult> {
        let response = self
            .http
            .get(self.app_url.join("/api/cli/wizard-session/poll")?)
            .query(&[("session_token", session_token)])
            .bearer_auth(poll_token)
            .send()
            .await
            .map_err(api_error)?;
        Ok(response.json().await?)
    }
    pub fn is_retryable(error: &anyhow::Error) -> bool {
        error.downcast_ref::<HttpError>().is_none_or(|error| {
            error.status.is_server_error() || matches!(error.status.as_u16(), 408 | 429)
        })
    }
}

#[non_exhaustive]
pub struct OAuthEndpoints {
    pub authorize_url: url::Url,
    pub token_url: url::Url,
}
impl OAuthClient {
    pub fn endpoints(api_url: &str) -> Result<OAuthEndpoints> {
        Ok(OAuthEndpoints {
            authorize_url: url::Url::parse(&format!(
                "{}/oauth/authorize",
                api_url.trim_end_matches('/')
            ))?,
            token_url: url::Url::parse(&format!("{}/oauth/token", api_url.trim_end_matches('/')))?,
        })
    }
}
