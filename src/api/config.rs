use std::{fmt, path::PathBuf, time::Duration};

pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

/// Explicit configuration for direct API calls. Construction does not log in.
#[derive(Clone)]
pub struct Config {
    pub(crate) api_key: String,
    pub api_url: String,
    pub app_url: String,
    pub org_name: Option<String>,
    pub timeout: Duration,
    /// Explicit execution context. None owns a dedicated runtime; ambient Tokio
    /// contexts are never selected automatically. A supplied runtime must remain
    /// alive and driven until its accepted operations and shutdown complete.
    pub runtime: Option<tokio::runtime::Handle>,
    pub user_agent: Option<String>,
    /// An explicit PEM file; mutually exclusive with `custom_ca_bundle_pem`.
    pub custom_ca_bundle_path: Option<PathBuf>,
    /// Raw PEM data; otherwise the existing environment CA convention applies.
    pub custom_ca_bundle_pem: Option<Vec<u8>>,
}
impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            api_url: DEFAULT_API_URL.into(),
            app_url: DEFAULT_APP_URL.into(),
            org_name: None,
            timeout: Duration::from_secs(30),
            runtime: None,
            user_agent: None,
            custom_ca_bundle_path: None,
            custom_ca_bundle_pem: None,
        }
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Endpoints may be invalid and contain credentials before validation.
        f.debug_struct("Config")
            .field("api_key", &"[redacted]")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}
