use std::{env, ops::Deref};

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

use crate::GeneratedClient;

/// Default endpoint for the Braintrust REST API.
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

/// An authenticated Braintrust REST API client.
///
/// All generated operations are available directly on this client. Its
/// authorization header is marked sensitive to keep the key out of Debug output.
#[derive(Clone, Debug)]
pub struct Client {
    inner: GeneratedClient,
}

/// Errors encountered while configuring an authenticated API client.
#[derive(Debug, thiserror::Error)]
pub enum ClientInitError {
    #[error("BRAINTRUST_API_KEY is not set")]
    MissingApiKey,
    #[error("API key must be non-empty and valid in an HTTP authorization header")]
    InvalidApiKey,
    #[error("{0} must contain valid UTF-8")]
    InvalidEnvironment(&'static str),
    #[error("failed to create HTTP client: {0}")]
    Http(#[from] reqwest::Error),
}

impl Client {
    /// Read `BRAINTRUST_API_KEY` and construct an authenticated client.
    ///
    /// `BRAINTRUST_API_URL` optionally overrides [`DEFAULT_API_URL`].
    pub fn new() -> Result<Self, ClientInitError> {
        let key = match env::var("BRAINTRUST_API_KEY") {
            Ok(key) => key,
            Err(env::VarError::NotPresent) => return Err(ClientInitError::MissingApiKey),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ClientInitError::InvalidEnvironment("BRAINTRUST_API_KEY"));
            }
        };
        Self::with_api_key(key)
    }

    /// Use an explicit API key instead of reading `BRAINTRUST_API_KEY`.
    ///
    /// `BRAINTRUST_API_URL` optionally overrides [`DEFAULT_API_URL`].
    pub fn with_api_key(api_key: impl AsRef<str>) -> Result<Self, ClientInitError> {
        let url = match env::var("BRAINTRUST_API_URL") {
            Ok(url) => url,
            Err(env::VarError::NotPresent) => DEFAULT_API_URL.to_owned(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ClientInitError::InvalidEnvironment("BRAINTRUST_API_URL"));
            }
        };
        Self::with_api_key_and_url(api_key, url)
    }

    /// Use an explicit API key and endpoint without reading environment variables.
    pub fn with_api_key_and_url(
        api_key: impl AsRef<str>,
        api_url: impl AsRef<str>,
    ) -> Result<Self, ClientInitError> {
        let api_key = api_key.as_ref();
        if api_key.trim().is_empty() {
            return Err(ClientInitError::InvalidApiKey);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| ClientInitError::InvalidApiKey)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let builder = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ));
        // Preserve the generated constructor's native timeout defaults.
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(15));
        let http = builder.build()?;
        Ok(Self {
            inner: GeneratedClient::new_with_client(api_url.as_ref().trim_end_matches('/'), http),
        })
    }
}

impl Deref for Client {
    type Target = GeneratedClient;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
