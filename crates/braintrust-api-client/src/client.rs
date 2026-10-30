use std::env;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

use crate::Client;

/// Default endpoint for the Braintrust REST API.
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

/// Configuration for an authenticated Braintrust REST API client.
///
/// Unset values are read from `BRAINTRUST_API_KEY` and `BRAINTRUST_API_URL`
/// when [`build`](Self::build) is called.
#[derive(Default)]
pub struct ClientBuilder {
    api_key: Option<String>,
    api_url: Option<String>,
    http_client_builder: Option<reqwest::ClientBuilder>,
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
    /// Configure bearer authentication using explicit values or environment variables.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }
}

impl ClientBuilder {
    /// Override `BRAINTRUST_API_KEY`.
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Override `BRAINTRUST_API_URL` and the default API endpoint.
    pub fn api_url(mut self, api_url: impl Into<String>) -> Self {
        self.api_url = Some(api_url.into());
        self
    }

    /// Use custom transport settings, preserving their timeouts, headers, and user agent.
    ///
    /// The API key overrides any default authorization header on this builder.
    pub fn http_client_builder(mut self, builder: reqwest::ClientBuilder) -> Self {
        self.http_client_builder = Some(builder);
        self
    }

    /// Build the generated client with a sensitive bearer authorization header.
    pub fn build(self) -> Result<Client, ClientInitError> {
        let api_key = match self.api_key {
            Some(key) => key,
            None => match env::var("BRAINTRUST_API_KEY") {
                Ok(key) => key,
                Err(env::VarError::NotPresent) => return Err(ClientInitError::MissingApiKey),
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(ClientInitError::InvalidEnvironment("BRAINTRUST_API_KEY"));
                }
            },
        };
        let api_url = match self.api_url {
            Some(url) => url,
            None => match env::var("BRAINTRUST_API_URL") {
                Ok(url) => url,
                Err(env::VarError::NotPresent) => DEFAULT_API_URL.to_owned(),
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(ClientInitError::InvalidEnvironment("BRAINTRUST_API_URL"));
                }
            },
        };

        if api_key.trim().is_empty() {
            return Err(ClientInitError::InvalidApiKey);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| ClientInitError::InvalidApiKey)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let builder = self.http_client_builder.unwrap_or_else(|| {
            let builder = reqwest::Client::builder().user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ));
            // Preserve the generated constructor's native timeout defaults.
            #[cfg(not(target_arch = "wasm32"))]
            let builder = builder
                .connect_timeout(std::time::Duration::from_secs(15))
                .timeout(std::time::Duration::from_secs(15));
            builder
        });
        let http = builder.default_headers(headers).build()?;
        Ok(Client::new_with_client(api_url.trim_end_matches('/'), http))
    }
}
