//! Process-wide SDK configuration.
use crate::{BraintrustError, Result, ServiceRuntime};
use std::sync::OnceLock;

/// Process-wide SDK configuration.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct ServiceConfig {
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

static CONFIG: OnceLock<ServiceConfig> = OnceLock::new();

/// Set SDK configuration before the first client or uploader is used.
/// Configuration is applied on first use without starting the SDK immediately.
/// Repeating identical configuration is allowed. Conflicting configuration fails.
pub fn configure_service(config: ServiceConfig) -> Result<()> {
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
