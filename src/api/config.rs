use std::{fmt, path::PathBuf, time::Duration};

pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

/// Explicit configuration for direct API calls. Construction does not log in.
#[derive(Clone)]
pub struct Config {
    pub(crate) api_key: String,
    pub(crate) api_url: String,
    pub(crate) app_url: String,
    pub(crate) org_name: Option<String>,
    pub(crate) timeout: Duration,
    pub(crate) user_agent: Option<String>,
    /// An explicit PEM file; mutually exclusive with `custom_ca_bundle_pem`.
    pub(crate) custom_ca_bundle_path: Option<PathBuf>,
    /// Raw PEM data; otherwise the existing environment CA convention applies.
    pub(crate) custom_ca_bundle_pem: Option<Vec<u8>>,
}
impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            api_url: DEFAULT_API_URL.into(),
            app_url: DEFAULT_APP_URL.into(),
            org_name: None,
            timeout: Duration::from_secs(30),
            user_agent: None,
            custom_ca_bundle_path: None,
            custom_ca_bundle_pem: None,
        }
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
    /// Set the api url.
    pub fn set_api_url(&mut self, value: impl Into<String>) -> &mut Self {
        self.api_url = value.into();
        self
    }
    /// Return the app url.
    pub fn app_url(&self) -> &str {
        &self.app_url
    }
    /// Set the app url.
    pub fn set_app_url(&mut self, value: impl Into<String>) -> &mut Self {
        self.app_url = value.into();
        self
    }
    /// Return the org name.
    pub fn org_name(&self) -> Option<&str> {
        self.org_name.as_deref()
    }
    /// Set the org name.
    pub fn set_org_name(&mut self, value: Option<String>) -> &mut Self {
        self.org_name = value;
        self
    }
    /// Return the timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    /// Set the timeout.
    pub fn set_timeout(&mut self, value: Duration) -> &mut Self {
        self.timeout = value;
        self
    }
    /// Return the user agent.
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }
    /// Set the user agent.
    pub fn set_user_agent(&mut self, value: Option<String>) -> &mut Self {
        self.user_agent = value;
        self
    }
    /// Return the custom ca bundle path.
    pub fn custom_ca_bundle_path(&self) -> Option<&PathBuf> {
        self.custom_ca_bundle_path.as_ref()
    }
    /// Set the custom ca bundle path.
    pub fn set_custom_ca_bundle_path(&mut self, value: Option<PathBuf>) -> &mut Self {
        self.custom_ca_bundle_path = value;
        self
    }
    /// Return the custom ca bundle pem.
    pub fn custom_ca_bundle_pem(&self) -> Option<&Vec<u8>> {
        self.custom_ca_bundle_pem.as_ref()
    }
    /// Set the custom ca bundle pem.
    pub fn set_custom_ca_bundle_pem(&mut self, value: Option<Vec<u8>>) -> &mut Self {
        self.custom_ca_bundle_pem = value;
        self
    }
}
