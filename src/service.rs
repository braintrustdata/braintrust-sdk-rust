//! Process-wide SDK configuration.
use crate::{BraintrustError, Result, ServiceRuntime};
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Process-wide SDK limits. Configure before constructing the first SDK facade.
/// Additional process and organization budgets are opt-in.
/// Per-client retained work remains bounded by its configured queue capacity.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct ServiceConfig {
    /// Maximum simultaneous SDK HTTP requests across every client, including
    /// uploads and API calls. Defaults to `BRAINTRUST_MAX_CONCURRENT_LOG_REQUESTS`
    /// when set to a positive number, otherwise 8.
    #[builder(default = default_max_concurrent_requests())]
    pub max_concurrent_requests: usize,
    /// Maximum retained rows across all clients, including in-flight work.
    pub max_rows: Option<usize>,
    /// Maximum retained rows for one organization across client facades.
    pub max_org_rows: Option<usize>,
    /// Maximum entries in each shared project and server-capability cache.
    #[builder(default = 256)]
    pub max_cached_resolutions: usize,
    /// Maximum duration of one [`crate::TokenProvider::token`] call.
    #[builder(default = Duration::from_secs(30))]
    pub token_provider_timeout: Duration,
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
        || config.max_cached_resolutions == 0
        || config.token_provider_timeout.is_zero()
    {
        return Err(BraintrustError::InvalidConfig(
            "service concurrency, cache capacity, and preparation timeout must be positive and supported".into(),
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

pub(crate) struct Service {
    pub engine: Arc<crate::log_queue::engine::Engine>,
    pub attempts: Arc<crate::scheduler::Scheduler>,
    pub config: ServiceConfig,
    pub projects: crate::resolution::Cache<crate::resolution::ProjectKey, String>,
    pub capabilities: crate::resolution::Cache<crate::resolution::CapabilityKey, (usize, bool)>,
}
impl std::ops::Deref for Service {
    type Target = crate::log_queue::engine::Engine;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}
impl Service {
    pub fn spawn_operation<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        crate::runtime::handle()
            .expect("SDK runtime is available")
            .spawn(future)
    }
}
pub(crate) fn global() -> Result<&'static Service> {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    if let Some(service) = SERVICE.get() {
        return Ok(service);
    }
    crate::runtime::handle()?;
    Ok(SERVICE.get_or_init(|| {
        let config = config().clone();
        let cache_capacity = config.max_cached_resolutions;
        let engine = Arc::new(crate::log_queue::engine::Engine::new(config.clone()));
        engine.start_jobs();
        Service {
            engine,
            attempts: crate::scheduler::Scheduler::new(config.max_concurrent_requests),
            config,
            projects: crate::resolution::Cache::new(cache_capacity),
            capabilities: crate::resolution::Cache::new(cache_capacity),
        }
    }))
}
