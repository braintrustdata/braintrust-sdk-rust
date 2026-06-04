use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

// 30s covers both quick API calls (login, project registration) and slower batch log uploads.
// The TypeScript SDK applies no explicit timeout.
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct ApiHttpConfig {
    pub(crate) request_timeout: Duration,
    pub(crate) login_timeout: Duration,
    pub(crate) ca_bundle: Option<PathBuf>,
}

impl Default for ApiHttpConfig {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            login_timeout: DEFAULT_LOGIN_TIMEOUT,
            ca_bundle: None,
        }
    }
}

pub(crate) mod env {
    pub(crate) fn api_key() -> Option<String> {
        std::env::var("BRAINTRUST_API_KEY").ok()
    }

    pub(crate) fn app_url() -> Option<String> {
        std::env::var("BRAINTRUST_APP_URL").ok()
    }

    pub(crate) fn api_url() -> Option<String> {
        std::env::var("BRAINTRUST_API_URL").ok()
    }

    pub(crate) fn org_name() -> Option<String> {
        std::env::var("BRAINTRUST_ORG_NAME").ok()
    }

    pub(crate) fn default_project() -> Option<String> {
        std::env::var("BRAINTRUST_DEFAULT_PROJECT").ok()
    }
}
