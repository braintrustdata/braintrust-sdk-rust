//! Process-wide executor and resources; facades never own a runtime.
use crate::{BraintrustError, Result};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Process-wide SDK limits. Configure before constructing the first SDK facade.
/// Additional process, organization, and destination budgets are opt-in.
/// Per-client retained work remains bounded by its configured queue capacity.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
pub struct ServiceConfig {
    /// Maximum simultaneous ingestion HTTP attempts, including overflow uploads.
    #[builder(default = 8)]
    pub max_concurrent_requests: usize,
    /// Maximum simultaneous destination preparation operations.
    #[builder(default = 16)]
    pub max_concurrent_preparations: usize,
    /// Maximum retained rows across all clients, including in-flight work.
    pub max_rows: Option<usize>,
    /// Maximum estimated retained payload bytes across all clients.
    pub max_bytes: Option<usize>,
    /// Maximum retained rows for one client and its derived handles.
    pub max_client_rows: Option<usize>,
    /// Maximum retained rows for one organization across client facades.
    pub max_org_rows: Option<usize>,
    /// Maximum retained rows for one destination across client facades.
    /// Unresolved names and explicit IDs have separate admission identities.
    pub max_destination_rows: Option<usize>,
    /// Maximum entries in each shared project and server-capability cache.
    #[builder(default = 256)]
    pub max_cached_resolutions: usize,
    /// Maximum duration of one credential provider or preparation operation.
    #[builder(default = Duration::from_secs(30))]
    pub preparation_timeout: Duration,
}
impl Default for ServiceConfig {
    fn default() -> Self {
        Self::builder()
            .max_concurrent_requests(
                std::env::var("BRAINTRUST_MAX_CONCURRENT_LOG_REQUESTS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(8),
            )
            .build()
    }
}
static CONFIG: Mutex<Option<ServiceConfig>> = Mutex::new(None);
static SERVICE: OnceLock<std::result::Result<Service, String>> = OnceLock::new();

/// Set process-wide limits before the first SDK client or uploader is built.
/// Repeating identical configuration is allowed; conflicting late configuration fails.
pub fn configure_service(config: ServiceConfig) -> Result<()> {
    if config.max_concurrent_requests == 0
        || config.max_concurrent_preparations == 0
        || config.max_concurrent_preparations > tokio::sync::Semaphore::MAX_PERMITS
        || config.max_cached_resolutions == 0
        || config.preparation_timeout.is_zero()
    {
        return Err(BraintrustError::InvalidConfig(
            "service concurrency, cache capacity, and preparation timeout must be positive and supported".into(),
        ));
    }
    let mut configured = CONFIG.lock().unwrap();
    if let Some(existing) = configured.as_ref() {
        if existing != &config {
            return Err(BraintrustError::InvalidConfig(
                "SDK service configuration is already fixed".into(),
            ));
        }
    } else {
        *configured = Some(config);
    }
    Ok(())
}

pub(crate) struct Service {
    pub handle: tokio::runtime::Handle,
    pub engine: Arc<crate::log_queue::engine::Engine>,
    pub attempts: Arc<crate::scheduler::Scheduler>,
    pub preparations: Arc<tokio::sync::Semaphore>,
    pub config: ServiceConfig,
    pub projects: crate::resolution::Cache<crate::resolution::ProjectKey, String>,
    pub capabilities: crate::resolution::Cache<crate::resolution::RouteKey, (usize, bool)>,
}
impl std::ops::Deref for Service {
    type Target = crate::log_queue::engine::Engine;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}
pub(crate) fn global() -> Result<&'static Service> {
    SERVICE
        .get_or_init(|| {
            let config = {
                let mut configured = CONFIG.lock().unwrap();
                configured
                    .get_or_insert_with(ServiceConfig::default)
                    .clone()
            };
            let engine = Arc::new(crate::log_queue::engine::Engine::new(config.clone()));
            let worker = engine.clone();
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("braintrust-delivery".into())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .max_blocking_threads(4)
                        .build();
                    match runtime {
                        Ok(runtime) => {
                            let _ = sender.send(Ok(runtime.handle().clone()));
                            runtime.block_on(worker.run());
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error.to_string()));
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            let handle = receiver.recv().map_err(|e| e.to_string())??;
            let cache_capacity = config.max_cached_resolutions;
            Ok(Service {
                handle,
                engine,
                attempts: crate::scheduler::Scheduler::new(config.max_concurrent_requests),
                preparations: Arc::new(tokio::sync::Semaphore::new(
                    config.max_concurrent_preparations,
                )),
                config,
                projects: crate::resolution::Cache::new(cache_capacity),
                capabilities: crate::resolution::Cache::new(cache_capacity),
            })
        })
        .as_ref()
        .map_err(|error| BraintrustError::Background(error.clone()))
}

/// Execute on the service runtime, so connection lifetimes never depend on callers.
pub(crate) async fn execute<T: Send + 'static>(
    operation: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    global()?
        .handle
        .spawn(operation)
        .await
        .map_err(|e| BraintrustError::Background(e.to_string()))?
}

/// Issue an owned operation immediately, independently of whether its completion is observed.
pub(crate) fn submit<T, E>(
    operation: impl Future<Output = std::result::Result<T, E>> + Send + 'static,
) -> crate::Completion<T, E>
where
    T: Send + 'static,
    E: From<crate::ExecutionError> + Send + 'static,
{
    let (sender, mut completion) = crate::Completion::pair();
    match global() {
        Ok(service) => {
            service.handle.spawn(async move {
                let _ = sender.send(operation.await);
            });
        }
        Err(_) => {
            completion.record_issuance(Err(crate::ExecutionError::WorkerLost.into()));
        }
    }
    completion
}
