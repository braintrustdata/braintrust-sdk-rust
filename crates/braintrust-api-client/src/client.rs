use std::env;

use crate::transport::LoginOrganization;
use reqwest::header::{AUTHORIZATION, HeaderValue};

/// Client with Braintrust authentication, discovery and upload support.
#[derive(Clone)]
pub struct Client {
    pub(crate) baseurl: String,
    pub(crate) client: reqwest::Client,
    pub(crate) inner: ClientContext,
    generated: crate::GeneratedClient,
}

impl std::ops::Deref for Client {
    type Target = crate::GeneratedClient;
    fn deref(&self) -> &Self::Target {
        &self.generated
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("api_url", &self.baseurl)
            .finish_non_exhaustive()
    }
}

/// Default endpoint for the Braintrust REST API.
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";
/// Default endpoint for authentication and control-plane operations.
pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";

/// Authentication, organization and upload settings held by [`Client`].
#[derive(Clone, Debug)]
pub struct ClientContext {
    pub(crate) app_url: String,
    authorization: Option<HeaderValue>,
    pub(crate) org_name: Option<HeaderValue>,
    pub(crate) organization: Option<LoginOrganization>,
    pub(crate) upload_client: reqwest::Client,
}

impl Default for ClientContext {
    fn default() -> Self {
        Self {
            app_url: DEFAULT_APP_URL.to_owned(),
            authorization: None,
            org_name: None,
            organization: None,
            upload_client: reqwest::Client::new(),
        }
    }
}

pub(crate) fn authorize_request(context: &ClientContext, request: &mut reqwest::Request) {
    if let Some(authorization) = &context.authorization {
        request
            .headers_mut()
            .insert(AUTHORIZATION, authorization.clone());
    }
}

/// Configuration for a Braintrust API client.
///
/// Unset key and URLs are read from `BRAINTRUST_API_KEY`, `BRAINTRUST_API_URL`,
/// and `BRAINTRUST_APP_URL` when the client is built.
#[derive(Default)]
pub struct ClientBuilder {
    api_key: Option<String>,
    api_url: Option<String>,
    app_url: Option<String>,
    org_name: Option<String>,
    http_client_builder: Option<reqwest::ClientBuilder>,
    upload_http_client_builder: Option<reqwest::ClientBuilder>,
}

/// Errors encountered while configuring or logging in an API client.
#[derive(Debug, thiserror::Error)]
pub enum ClientInitError {
    #[error("BRAINTRUST_API_KEY is not set")]
    MissingApiKey,
    #[error("API key must be non-empty and valid in an HTTP authorization header")]
    InvalidApiKey,
    #[error("organization name must be valid in an HTTP header")]
    InvalidOrgName,
    #[error("{0} must contain valid UTF-8")]
    InvalidEnvironment(&'static str),
    #[error("{0} must be an absolute HTTP or HTTPS URL")]
    InvalidUrl(&'static str),
    #[error("no organizations are available for this API key")]
    NoOrganizations,
    #[error("organization '{0}' was not returned by login")]
    OrganizationNotFound(String),
    #[error("login failed: {0}")]
    Login(#[from] Box<crate::Error<serde_json::Value>>),
    #[error("failed to create HTTP client: {0}")]
    Http(#[from] reqwest::Error),
}

impl Client {
    /// The API endpoint used by generated methods, queries and logging.
    pub fn baseurl(&self) -> &str {
        &self.baseurl
    }
    /// Configure authentication, endpoints, and transport settings.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The organization selected by [`ClientBuilder::build_with_login`].
    pub fn organization(&self) -> Option<&LoginOrganization> {
        self.inner.organization.as_ref()
    }

    /// The endpoint used for login and control-plane operations.
    pub fn app_url(&self) -> &str {
        &self.inner.app_url
    }
}

impl ClientBuilder {
    /// Override `BRAINTRUST_API_KEY`.
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Override `BRAINTRUST_API_URL` and the default API endpoint.
    ///
    /// Login may replace this fallback with the selected organization's API URL.
    pub fn api_url(mut self, api_url: impl Into<String>) -> Self {
        self.api_url = Some(api_url.into());
        self
    }

    /// Override `BRAINTRUST_APP_URL` and the default app endpoint.
    pub fn app_url(mut self, app_url: impl Into<String>) -> Self {
        self.app_url = Some(app_url.into());
        self
    }

    /// Select an organization for login, version checks, and log submissions.
    pub fn org_name(mut self, org_name: impl Into<String>) -> Self {
        self.org_name = Some(org_name.into());
        self
    }

    /// Use custom API transport settings, preserving timeouts, headers, and user agent.
    ///
    /// The API key overrides any default authorization header on this builder.
    pub fn http_client_builder(mut self, builder: reqwest::ClientBuilder) -> Self {
        self.http_client_builder = Some(builder);
        self
    }

    /// Configure the separate transport for signed storage uploads.
    ///
    /// Use this for custom storage certificates, proxies, or timeouts. Do not add
    /// Braintrust authorization headers to this transport.
    pub fn upload_http_client_builder(mut self, builder: reqwest::ClientBuilder) -> Self {
        self.upload_http_client_builder = Some(builder);
        self
    }

    /// Construct a client without making network requests.
    pub fn build(self) -> Result<Client, ClientInitError> {
        let api_key = match self.api_key {
            Some(key) => key,
            None => env::var("BRAINTRUST_API_KEY").map_err(|error| match error {
                env::VarError::NotPresent => ClientInitError::MissingApiKey,
                env::VarError::NotUnicode(_) => {
                    ClientInitError::InvalidEnvironment("BRAINTRUST_API_KEY")
                }
            })?,
        };
        if api_key.trim().is_empty() {
            return Err(ClientInitError::InvalidApiKey);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| ClientInitError::InvalidApiKey)?;
        authorization.set_sensitive(true);
        let api_url = resolve_url(self.api_url, "BRAINTRUST_API_URL", DEFAULT_API_URL)?;
        let app_url = resolve_url(self.app_url, "BRAINTRUST_APP_URL", DEFAULT_APP_URL)?;
        let org_name = self
            .org_name
            .map(|name| HeaderValue::from_str(&name).map_err(|_| ClientInitError::InvalidOrgName))
            .transpose()?;
        let http = self
            .http_client_builder
            .unwrap_or_else(default_http_builder)
            .build()?;
        let upload_client = self
            .upload_http_client_builder
            .unwrap_or_else(default_http_builder)
            .build()?;
        let generated = crate::GeneratedClient::with_client(http.clone())
            .with_base_url(&api_url)
            .with_api_key(api_key)
            // Logging and query responses may exceed the generator's 8 MiB default.
            .with_max_response_body_bytes(usize::MAX);
        Ok(Client {
            baseurl: api_url,
            client: http,
            generated,
            inner: ClientContext {
                app_url,
                authorization: Some(authorization),
                org_name,
                organization: None,
                upload_client,
            },
        })
    }

    /// Log in and use the selected organization's API endpoint.
    ///
    /// Selects the first organization when no name was provided. If the server
    /// omits its API URL, retains the configured API endpoint. Discovery happens
    /// before returning the client; existing clones are never retargeted.
    pub async fn build_with_login(self) -> Result<Client, ClientInitError> {
        let org_name = self.org_name.clone();
        let mut client = self.build()?;
        let response = client.login().await.map_err(Box::new)?.into_inner();
        let organization = if let Some(name) = org_name {
            response
                .org_info
                .into_iter()
                .find(|org| org.name == name)
                .ok_or(ClientInitError::OrganizationNotFound(name))?
        } else {
            response
                .org_info
                .into_iter()
                .next()
                .ok_or(ClientInitError::NoOrganizations)?
        };
        if let Some(url) = &organization.api_url {
            client.baseurl = validate_url(url, "login API URL")?;
            client.generated = client.generated.with_base_url(&client.baseurl);
        }
        client.inner.org_name = Some(
            HeaderValue::from_str(&organization.name)
                .map_err(|_| ClientInitError::InvalidOrgName)?,
        );
        client.inner.organization = Some(organization);
        Ok(client)
    }
}

fn resolve_url(
    value: Option<String>,
    variable: &'static str,
    fallback: &str,
) -> Result<String, ClientInitError> {
    let value = match value {
        Some(value) => value,
        None => match env::var(variable) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) => fallback.to_owned(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ClientInitError::InvalidEnvironment(variable));
            }
        },
    };
    validate_url(&value, variable)
}

fn validate_url(value: &str, field: &'static str) -> Result<String, ClientInitError> {
    let url = reqwest::Url::parse(value).map_err(|_| ClientInitError::InvalidUrl(field))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ClientInitError::InvalidUrl(field));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn default_http_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder().user_agent(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ));
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(15));
    builder
}
