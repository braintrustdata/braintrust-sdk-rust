//! Process-wide executor and resources; facades never own a runtime.
use crate::{BraintrustError, Result};
use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::runtime::Handle;

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
    /// The Tokio runtime that runs SDK work. See [`ServiceRuntime`].
    #[builder(default)]
    pub runtime: ServiceRuntime,
}

/// Where the SDK runs its background work: delivery, uploads and API calls.
///
/// The runtime must have I/O and timers enabled. A current-thread runtime only
/// makes progress while some thread drives it, so delivery stalls, and
/// [`crate::Completion::blocking_wait`] can wait forever, when nothing does.
///
/// If the runtime shuts down, delivery restarts straight away on the SDK's own
/// thread, and a stopped runtime is never selected again. Rows that were being
/// uploaded go back in the queue, so rows from a request that was in flight may
/// be sent twice; Logs3 writes by row ID, so a repeat overwrites the same row.
/// Other operations still pending on the stopped runtime fail with
/// [`crate::ExecutionError::WorkerLost`].
///
/// Without explicit configuration, `BRAINTRUST_SERVICE_RUNTIME=dedicated`
/// selects [`ServiceRuntime::Dedicated`]. Test suites usually want it: each
/// `#[tokio::test]` has its own runtime, and when the captured one ends, other
/// tests' pending operations fail.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ServiceRuntime {
    /// The runtime that is current when the first SDK client is built, or the
    /// SDK's own thread when there is none. The default.
    Current,
    /// Always the SDK's own single-threaded runtime on a dedicated thread.
    Dedicated,
    /// A runtime the application provides and keeps running.
    Handle(Handle),
}
impl Default for ServiceRuntime {
    fn default() -> Self {
        // Unit tests run concurrently on short-lived runtimes; sharing one that
        // stops mid-test would fail unrelated tests.
        if cfg!(test) {
            Self::Dedicated
        } else {
            Self::Current
        }
    }
}
impl ServiceRuntime {
    /// The default, unless `BRAINTRUST_SERVICE_RUNTIME` names another selection.
    fn from_env() -> Self {
        match std::env::var("BRAINTRUST_SERVICE_RUNTIME").as_deref() {
            Ok("dedicated") => Self::Dedicated,
            Ok("current") => Self::Current,
            _ => Self::default(),
        }
    }
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
        Self::builder()
            .max_concurrent_requests(
                std::env::var("BRAINTRUST_MAX_CONCURRENT_LOG_REQUESTS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(8),
            )
            .runtime(ServiceRuntime::from_env())
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
    executor: Mutex<Executor>,
    pub engine: Arc<crate::log_queue::engine::Engine>,
    pub attempts: Arc<crate::scheduler::Scheduler>,
    pub preparations: Arc<tokio::sync::Semaphore>,
    pub config: ServiceConfig,
    pub projects: crate::resolution::Cache<crate::resolution::ProjectKey, String>,
    pub capabilities: crate::resolution::Cache<crate::resolution::CapabilityKey, (usize, bool)>,
}
/// The runtime currently hosting the delivery loop.
struct Executor {
    handle: Handle,
    /// Cleared when the delivery loop is dropped, normally by its runtime shutting down.
    running: Arc<AtomicBool>,
    /// Runtimes that shut down while hosting the service; never selected again.
    stopped: HashSet<tokio::runtime::Id>,
}
/// Dropped with the delivery loop. Restarts delivery straight away, because
/// callers may only be awaiting rows that are now back in the queue.
struct Running(Arc<AtomicBool>);
impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
        // A loop dropped while this thread is starting one is retried there.
        if !STARTING.get() {
            if let Some(Ok(service)) = SERVICE.get() {
                service.handle();
            }
        }
    }
}
thread_local! {
    /// Set while this thread starts a delivery loop. Spawning onto a runtime
    /// that is shutting down drops the loop at once, on this thread.
    static STARTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
impl std::ops::Deref for Service {
    type Target = crate::log_queue::engine::Engine;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}
impl Service {
    /// The runtime hosting SDK work, restarting the service on another one if
    /// the previous runtime shut down.
    pub fn handle(&self) -> Handle {
        let mut executor = self.executor.lock().unwrap();
        while !executor.running.load(Ordering::Acquire) {
            let stopped = executor.handle.id();
            tracing::warn!(runtime = %stopped, "SDK runtime stopped; restarting delivery on another runtime");
            executor.stopped.insert(stopped);
            let stopped = std::mem::take(&mut executor.stopped);
            match Executor::start(&self.config.runtime, &self.engine, stopped) {
                Ok(started) => *executor = started,
                Err((error, stopped)) => {
                    executor.stopped = stopped;
                    tracing::error!(%error, "failed to restart SDK delivery");
                    break;
                }
            }
        }
        executor.handle.clone()
    }
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle().spawn(future)
    }
}
impl Executor {
    fn start(
        runtime: &ServiceRuntime,
        engine: &Arc<crate::log_queue::engine::Engine>,
        stopped: HashSet<tokio::runtime::Id>,
    ) -> std::result::Result<Self, (String, HashSet<tokio::runtime::Id>)> {
        let current = || {
            Handle::try_current()
                .ok()
                .filter(|handle| !stopped.contains(&handle.id()))
        };
        let handle = match runtime {
            ServiceRuntime::Handle(handle) if !stopped.contains(&handle.id()) => Ok(handle.clone()),
            ServiceRuntime::Handle(_) => {
                tracing::warn!("configured SDK runtime shut down; falling back to another runtime");
                current().map(Ok).unwrap_or_else(dedicated)
            }
            ServiceRuntime::Current => current().map(Ok).unwrap_or_else(dedicated),
            ServiceRuntime::Dedicated => dedicated(),
        };
        let handle = match handle {
            Ok(handle) => handle,
            Err(error) => return Err((error, stopped)),
        };
        let running = Arc::new(AtomicBool::new(true));
        let guard = Running(running.clone());
        let worker = engine.clone();
        let starting = STARTING.replace(true);
        handle.spawn(async move {
            let _guard = guard;
            worker.run().await
        });
        STARTING.set(starting);
        Ok(Self {
            handle,
            running,
            stopped,
        })
    }
}
/// The SDK-owned runtime: one thread, started on first use and never stopped.
fn dedicated() -> std::result::Result<Handle, String> {
    static DEDICATED: OnceLock<std::result::Result<Handle, String>> = OnceLock::new();
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
            let executor = Executor::start(&config.runtime, &engine, HashSet::new())
                .map_err(|(error, _)| error)?;
            let cache_capacity = config.max_cached_resolutions;
            Ok(Service {
                executor: Mutex::new(executor),
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
        .map_err(|error| BraintrustError::Background(error.clone()))?;
    // Restart delivery promptly when its runtime has gone, even for calls that
    // only enqueue work.
    service.handle();
    Ok(service)
}

/// Execute on the service runtime, so connection lifetimes never depend on callers.
pub(crate) async fn execute<T: Send + 'static>(
    operation: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    global()?
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
            service.spawn(async move {
                let _ = sender.send(operation.await);
            });
        }
        Err(_) => {
            completion.record_issuance(Err(crate::ExecutionError::WorkerLost.into()));
        }
    }
    completion
}
