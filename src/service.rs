//! Process-wide SDK configuration.
use crate::{BraintrustError, Result, ServiceRuntime};
use std::sync::OnceLock;

/// Process-wide SDK configuration.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct ServiceConfig {
    /// Maximum simultaneous log uploads, project registrations, and login
    /// requests. Defaults to `BRAINTRUST_MAX_CONCURRENT_LOG_REQUESTS`
    /// when set to a positive number, otherwise 8.
    #[builder(default = default_max_concurrent_requests())]
    pub max_concurrent_requests: usize,
    /// Target capacity for each shared project and server-capability cache.
    /// Entries in use may temporarily exceed this limit.
    #[builder(default = 256)]
    pub max_cached_resolutions: usize,
    /// The Tokio runtime that runs SDK work. See [`ServiceRuntime`].
    #[builder(default)]
    pub runtime: ServiceRuntime,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

fn default_max_concurrent_requests() -> usize {
    std::env::var("BRAINTRUST_MAX_CONCURRENT_LOG_REQUESTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| (1..=tokio::sync::Semaphore::MAX_PERMITS).contains(v))
        .unwrap_or(8)
}

static CONFIG: OnceLock<ServiceConfig> = OnceLock::new();

/// Set SDK configuration before the first client or uploader is used.
/// Configuration is applied on first use without starting the SDK immediately.
/// Repeating identical configuration is allowed. Conflicting configuration fails.
pub fn configure_service(config: ServiceConfig) -> Result<()> {
    if config.max_concurrent_requests == 0
        || config.max_concurrent_requests > tokio::sync::Semaphore::MAX_PERMITS
    {
        return Err(BraintrustError::InvalidConfig(
            "service request concurrency must be positive and supported".into(),
        ));
    }
    if config.max_cached_resolutions == 0 {
        return Err(BraintrustError::InvalidConfig(
            "service cache capacity must be positive".into(),
        ));
    }
    if CONFIG.get_or_init(|| config.clone()) != &config {
        return Err(BraintrustError::InvalidConfig(
            "SDK service configuration is already fixed".into(),
        ));
    }
    Ok(())
}

pub(crate) fn config() -> &'static ServiceConfig {
    CONFIG.get_or_init(ServiceConfig::default)
}
