//! Process-wide executor and resources; facades never own a runtime.
use crate::{BraintrustError, Result};
use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::runtime::{Handle, Id as RuntimeId};

/// Process-wide SDK limits. Configure before constructing the first SDK facade.
/// Additional process and organization budgets are opt-in.
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

/// Where the SDK runs its work.
///
/// Every client shares one delivery service: one intake, set of partitions,
/// request scheduler, connection pool and set of caches for the process. This
/// selects the runtimes that poll its work: each operation a client method
/// starts, such as an API call, and each upload job. The SDK's own thread is
/// only a fallback for work started outside any runtime, such as from
/// synchronous code.
///
/// The runtime must have I/O and timers enabled. A current-thread runtime only
/// makes progress while its owner drives it, so keep driving it (for example,
/// by awaiting from inside it) until the work it started has finished.
///
/// If a runtime shuts down with SDK work unfinished, that work's rows go back in
/// the queue and upload from another runtime, and the stopped runtime is never
/// selected again. Rows from a request that got a response are never sent
/// again, but the server may have received the request that was in flight, so
/// its rows can arrive twice; Logs3 writes by row ID, so a repeat overwrites the
/// same row. Operations still running on a stopped runtime fail with
/// [`crate::ExecutionError::WorkerLost`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum ServiceRuntime {
    /// The default: share the application's runtimes.
    ///
    /// Each operation runs on the runtime current where it is started. Each
    /// upload job runs on the runtime current when its rows were submitted, or
    /// on another submitter's if that one has stopped. Work started outside any
    /// runtime runs on the SDK's thread.
    ///
    /// Work depends only on the runtime that started it, so runtimes that end
    /// while others keep using the SDK, like one per `#[tokio::test]`, stop
    /// only their own work.
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
    /// Runtimes that shut down while running SDK work; never selected again.
    stopped: Mutex<HashSet<RuntimeId>>,
    pub engine: Arc<crate::log_queue::engine::Engine>,
    pub attempts: Arc<crate::scheduler::Scheduler>,
    pub preparations: Arc<tokio::sync::Semaphore>,
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
    /// Spawn an operation a caller started: on the caller's runtime with
    /// [`ServiceRuntime::Current`], so it ends only with that runtime, or on the
    /// SDK's thread for callers outside any runtime.
    pub fn spawn_operation<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let caller = Handle::try_current().ok();
        match self.select(caller.as_slice().iter()) {
            Some(handle) => handle.spawn(future),
            // Only when the SDK's thread cannot start; the caller's runtime
            // is then the one place left to run.
            None => caller
                .unwrap_or_else(|| panic!("no Tokio runtime is available for SDK work"))
                .spawn(future),
        }
    }
    /// The runtime for an upload job: the latest submitter's that still runs.
    pub fn job_runtime<'a>(&self, submitters: impl Iterator<Item = &'a Handle>) -> Option<Handle> {
        self.select(submitters)
    }
    /// The configured runtime, else the first live application runtime among
    /// `candidates`, else the SDK's own thread.
    fn select<'a>(&self, mut candidates: impl Iterator<Item = &'a Handle>) -> Option<Handle> {
        let stopped = self.stopped.lock().unwrap();
        let live = |handle: &Handle| !stopped.contains(&handle.id());
        match &self.config.runtime {
            ServiceRuntime::Handle(handle) if live(handle) => return Some(handle.clone()),
            ServiceRuntime::Dedicated => return dedicated().ok(),
            ServiceRuntime::Handle(_) | ServiceRuntime::Current => {}
        }
        candidates
            .find(|handle| live(handle))
            .cloned()
            .or_else(|| dedicated().ok())
    }
    /// Record that `runtime` dropped unfinished SDK work, which only its
    /// shutdown does.
    pub fn runtime_stopped(&self, runtime: RuntimeId) {
        if !is_dedicated_id(runtime) && self.stopped.lock().unwrap().insert(runtime) {
            if matches!(&self.config.runtime, ServiceRuntime::Handle(h) if h.id() == runtime) {
                tracing::warn!("configured SDK runtime shut down; falling back to another runtime");
            }
            tracing::debug!(%runtime, "SDK runtime stopped; moving its work to another runtime");
        }
    }
}
fn is_dedicated_id(runtime: RuntimeId) -> bool {
    matches!(DEDICATED.get(), Some(Ok(dedicated)) if dedicated.id() == runtime)
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
                stopped: Mutex::new(HashSet::new()),
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
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| service.engine.start_jobs());
    Ok(service)
}

/// Execute on the service runtime, so connection lifetimes never depend on callers.
pub(crate) async fn execute<T: Send + 'static>(
    operation: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    global()?
        .spawn_operation(operation)
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
            service.spawn_operation(async move {
                let _ = sender.send(operation.await);
            });
        }
        Err(_) => {
            completion.record_issuance(Err(crate::ExecutionError::WorkerLost.into()));
        }
    }
    completion
}
