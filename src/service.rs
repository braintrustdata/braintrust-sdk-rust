//! Process-wide SDK configuration.
use crate::{BraintrustError, Result, ServiceRuntime};
use std::sync::OnceLock;

/// Process-wide SDK configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct ServiceConfig {
    /// The Tokio runtime that runs SDK work. See [`ServiceRuntime`].
    #[builder(default)]
    pub runtime: ServiceRuntime,
}

static CONFIG: OnceLock<ServiceConfig> = OnceLock::new();

/// Set SDK configuration before the first client or uploader is used.
/// Configuration is applied on first use without starting the SDK immediately.
/// Repeating identical configuration is allowed. Conflicting configuration fails.
pub fn configure_service(config: ServiceConfig) -> Result<()> {
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
