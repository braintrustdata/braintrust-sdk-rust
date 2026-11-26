//! Process-wide executor and resources; facades never own a runtime.
use crate::{BraintrustError, Result};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::runtime::{Handle, Id as RuntimeId};

/// Process-wide SDK limits. Configure before constructing the first SDK facade.
/// Additional process and organization budgets are opt-in.
/// Per-client retained work remains bounded by its configured queue capacity.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
pub struct ServiceConfig {
    /// Maximum simultaneous SDK HTTP requests across every client, including
    /// uploads and API calls. Defaults to `BRAINTRUST_MAX_CONCURRENT_LOG_REQUESTS`
    /// when set to a positive number, otherwise 8.
    #[builder(default = default_max_concurrent_requests())]
    pub max_concurrent_requests: usize,
    /// Maximum retained rows across all clients, including in-flight work.
    pub max_rows: Option<usize>,
    /// Maximum estimated retained payload bytes across all clients.
    pub max_bytes: Option<usize>,
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

/// Where the SDK runs its work.
///
/// The SDK selects one runtime when the process first uses the service.
/// The selected runtime must have I/O and timers enabled. Keep driving a
/// current-thread runtime until its SDK work finishes. If the selected
/// application runtime shuts down, the SDK moves pending rows to its own thread.
/// In-flight operations can fail with [`crate::ExecutionError::WorkerLost`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum ServiceRuntime {
    /// The default: use the runtime current at first use, or start the SDK's
    /// own runtime if there is none.
    #[default]
    Current,
    /// Everything runs on the SDK's own single-threaded runtime on a dedicated thread.
    Dedicated,
    /// Everything runs on a runtime the application provides and keeps running.
    Handle(Handle),
}
impl PartialEq for ServiceRuntime {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Current, Self::Current) | (Self::Dedicated, Self::Dedicated) => true,
            (Self::Handle(a), Self::Handle(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}
impl Eq for ServiceRuntime {}
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
static CONFIG: Mutex<Option<ServiceConfig>> = Mutex::new(None);
static SERVICE: OnceLock<std::result::Result<Service, String>> = OnceLock::new();

/// Set process-wide limits before the first SDK client or uploader is built.
/// The service initializes on first use. Setting configuration alone does not
/// start it or select the current runtime.
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
    runtime: Executor,
    pub engine: Arc<crate::log_queue::engine::Engine>,
    pub attempts: Arc<crate::scheduler::Scheduler>,
    pub config: ServiceConfig,
    pub projects: crate::resolution::Cache<crate::resolution::ProjectKey, String>,
    pub capabilities: crate::resolution::Cache<crate::resolution::CapabilityKey, (usize, bool)>,
}
/// One selected runtime with a dedicated fallback after shutdown.
struct Executor {
    primary: Option<Handle>,
    stopped: Arc<AtomicBool>,
}

struct RuntimeLifetime(Arc<AtomicBool>);

impl Drop for RuntimeLifetime {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Executor {
    fn new(config: &ServiceRuntime) -> std::result::Result<Self, String> {
        let primary = match config {
            ServiceRuntime::Current => Handle::try_current().ok(),
            ServiceRuntime::Dedicated => None,
            ServiceRuntime::Handle(handle) => Some(handle.clone()),
        };
        let stopped = Arc::new(AtomicBool::new(false));
        if let Some(handle) = &primary {
            // Capture the guard before spawning so it also drops if the
            // chosen runtime shuts down before the task is first polled.
            let lifetime = RuntimeLifetime(stopped.clone());
            handle.spawn(async move {
                let _lifetime = lifetime;
                std::future::pending::<()>().await;
            });
        } else {
            dedicated()?;
        }
        Ok(Self { primary, stopped })
    }
}

impl std::ops::Deref for Service {
    type Target = crate::log_queue::engine::Engine;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}
impl Service {
    pub fn runtime_handle(&self) -> Option<Handle> {
        if !self.runtime.stopped.load(Ordering::Acquire) {
            if let Some(handle) = &self.runtime.primary {
                return Some(handle.clone());
            }
        }
        dedicated().ok()
    }

    pub fn runtime_stopped(&self, id: RuntimeId) {
        if self
            .runtime
            .primary
            .as_ref()
            .is_some_and(|handle| handle.id() == id)
        {
            self.runtime.stopped.store(true, Ordering::Release);
        }
    }

    pub fn spawn_operation<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime_handle()
            .unwrap_or_else(|| panic!("no Tokio runtime is available for SDK work"))
            .spawn(future)
    }
}
static DEDICATED: OnceLock<std::result::Result<Handle, String>> = OnceLock::new();
/// The SDK-owned runtime: one thread, started on first use and never stopped.
fn dedicated() -> std::result::Result<Handle, String> {
    DEDICATED
        .get_or_init(|| {
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
                            runtime.block_on(std::future::pending::<()>());
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error.to_string()));
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            receiver.recv().map_err(|e| e.to_string())?
        })
        .clone()
}
/// The service, if it has been started, without starting it.
pub(crate) fn started() -> Option<&'static Service> {
    SERVICE.get()?.as_ref().ok()
}
pub(crate) fn global() -> Result<&'static Service> {
    let service = SERVICE
        .get_or_init(|| {
            let config = {
                let mut configured = CONFIG.lock().unwrap();
                configured
                    .get_or_insert_with(ServiceConfig::default)
                    .clone()
            };
            let engine = Arc::new(crate::log_queue::engine::Engine::new(config.clone()));
            let cache_capacity = config.max_cached_resolutions;
            Ok(Service {
                runtime: Executor::new(&config.runtime)?,
                engine,
                attempts: crate::scheduler::Scheduler::new(config.max_concurrent_requests),
                config,
                projects: crate::resolution::Cache::new(cache_capacity),
                capabilities: crate::resolution::Cache::new(cache_capacity),
            })
        })
        .as_ref()
        .map_err(|error| BraintrustError::Background(error.clone()))?;
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| service.engine.start_jobs());
    Ok(service)
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
            let panic = completion.panic_slot();
            service.spawn_operation(async move {
                use futures::FutureExt;
                match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                    Ok(result) => {
                        let _ = sender.send(result);
                    }
                    // Dropping the sender after storing the payload lets the
                    // waiter re-raise the panic instead of seeing WorkerLost.
                    Err(payload) => *panic.lock().unwrap() = Some(payload),
                }
            });
        }
        Err(_) => {
            completion.record_issuance(Err(crate::ExecutionError::WorkerLost.into()));
        }
    }
    completion
}
