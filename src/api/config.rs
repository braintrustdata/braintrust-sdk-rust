use std::{fmt, path::PathBuf, time::Duration};

pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

/// Explicit configuration for direct API calls. Construction does not log in.
#[derive(bon::Builder, Clone)]
pub struct Config {
    #[builder(into)]
    pub(crate) api_key: String,
    #[builder(default = DEFAULT_API_URL, into)]
    pub(crate) api_url: String,
    #[builder(default = DEFAULT_APP_URL, into)]
    pub(crate) app_url: String,
    #[builder(into)]
    pub(crate) org_name: Option<String>,
    #[builder(default = Duration::from_secs(30))]
    pub(crate) timeout: Duration,
    #[builder(into)]
    pub(crate) user_agent: Option<String>,
    /// An explicit PEM file; mutually exclusive with `custom_ca_bundle_pem`.
    pub(crate) custom_ca_bundle_path: Option<PathBuf>,
    /// Raw PEM data; otherwise the existing environment CA convention applies.
    pub(crate) custom_ca_bundle_pem: Option<Vec<u8>>,
}
impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::builder().api_key(api_key).build()
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("api_key", &"[redacted]")
            .field("api_url", &self.api_url)
            .field("app_url", &self.app_url)
            .field("org_name", &self.org_name)
            .field("timeout", &self.timeout)
            .field("user_agent", &self.user_agent)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// Return the api url.
    pub fn api_url(&self) -> &str {
        &self.api_url
    }
    /// Return the app url.
    pub fn app_url(&self) -> &str {
        &self.app_url
    }
    /// Return the org name.
    pub fn org_name(&self) -> Option<&str> {
        self.org_name.as_deref()
    }
    /// Return the timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    /// Return the user agent.
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }
    /// Return the custom ca bundle path.
    pub fn custom_ca_bundle_path(&self) -> Option<&PathBuf> {
        self.custom_ca_bundle_path.as_ref()
    }
    /// Return the custom ca bundle pem.
    pub fn custom_ca_bundle_pem(&self) -> Option<&Vec<u8>> {
        self.custom_ca_bundle_pem.as_ref()
    }
}
