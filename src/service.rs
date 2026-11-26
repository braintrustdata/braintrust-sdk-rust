//! Process-wide executor and resources; facades never own a runtime.
use crate::{BraintrustError, Result};
use std::collections::HashMap;
use std::future::Future;
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
/// Every client shares one delivery service: one intake, request scheduler,
/// set of budgets and set of caches for the process, with a queue per org.
/// This selects the runtimes that poll its work. Queues and connection pools
/// are kept per runtime, so each runtime polls only the work it started, and a
/// runtime that is alive but idle holds up nothing else. The SDK's own thread
/// is only a fallback for work started outside any runtime, such as from
/// synchronous code.
///
/// The runtime must have I/O and timers enabled. A current-thread runtime only
/// makes progress while its owner drives it, so keep driving it (for example,
/// by awaiting from inside it) until the work it started has finished.
///
/// If a runtime shuts down with SDK work unfinished, its queued rows upload
/// from the SDK's thread instead. Rows from a request that got a response are
/// never sent again, but the server may have received the request that was in
/// flight, so its rows can arrive twice; Logs3 writes by row ID, so a repeat
/// overwrites the same row. Operations still running on a stopped runtime fail
/// with [`crate::ExecutionError::WorkerLost`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum ServiceRuntime {
    /// The default: share the application's runtimes. Work runs on the runtime
    /// current where it is started, or on the SDK's thread outside any runtime.
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
        .filter(|v| *v > 0)
        .unwrap_or(8)
}
static CONFIG: Mutex<Option<ServiceConfig>> = Mutex::new(None);
static SERVICE: OnceLock<std::result::Result<Service, String>> = OnceLock::new();

/// Set process-wide limits before the first SDK client or uploader is built.
/// Repeating identical configuration is allowed; conflicting late configuration fails.
pub fn configure_service(config: ServiceConfig) -> Result<()> {
    if config.max_concurrent_requests == 0
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
    /// Application runtimes running SDK work. Each has a sentinel task that
    /// removes it when the runtime shuts down.
    lanes: Mutex<HashMap<RuntimeId, Handle>>,
    pub engine: Arc<crate::log_queue::engine::Engine>,
    pub attempts: Arc<crate::scheduler::Scheduler>,
    pub config: ServiceConfig,
    pub projects: crate::resolution::Cache<crate::resolution::ProjectKey, String>,
    pub capabilities: crate::resolution::Cache<crate::resolution::CapabilityKey, (usize, bool)>,
}
/// The runtime that polls a piece of SDK work: an application runtime, or
/// `None` for the SDK's own thread. Queues and connection pools are kept per
/// lane, so a runtime that is alive but idle only holds up its own work.
pub(crate) type Lane = Option<RuntimeId>;
/// A task parked on each lane's runtime; dropped when that runtime shuts down.
struct Sentinel(RuntimeId);
impl Drop for Sentinel {
    fn drop(&mut self) {
        if let Some(service) = started() {
            service.lane_stopped(self.0);
        }
    }
}
impl std::ops::Deref for Service {
    type Target = crate::log_queue::engine::Engine;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}
impl Service {
    /// The lane for work started here: the configured runtime, else (with
    /// [`ServiceRuntime::Current`]) the current one, else the SDK's thread.
    pub fn current_lane(&self) -> Lane {
        let handle = match &self.config.runtime {
            ServiceRuntime::Dedicated => return None,
            ServiceRuntime::Handle(handle) => handle.clone(),
            ServiceRuntime::Current => Handle::try_current().ok()?,
        };
        if is_dedicated(&handle) {
            return None;
        }
        let id = handle.id();
        if self
            .lanes
            .lock()
            .unwrap()
            .insert(id, handle.clone())
            .is_some()
        {
            return Some(id);
        }
        // Spawned without holding the lock: on a runtime that has shut down,
        // the sentinel is dropped at once and removes the lane again.
        handle.spawn(async move {
            let _sentinel = Sentinel(id);
            std::future::pending::<()>().await
        });
        self.lanes.lock().unwrap().contains_key(&id).then_some(id)
    }
    /// The runtime for `lane`, or the SDK's thread once that runtime has stopped.
    pub fn lane_handle(&self, lane: Lane) -> Option<Handle> {
        lane.and_then(|id| self.lanes.lock().unwrap().get(&id).cloned())
            .or_else(|| dedicated().ok())
    }
    /// Forget a runtime that shut down: its unfinished work moves to the SDK's thread.
    pub fn lane_stopped(&self, lane: RuntimeId) {
        if self.lanes.lock().unwrap().remove(&lane).is_some() {
            crate::http::forget_runtime(lane);
            tracing::debug!(runtime = %lane, "SDK runtime stopped; moving its work to the SDK's thread");
        }
    }
    /// Spawn an operation a caller started on the caller's lane, so it ends
    /// only with that runtime.
    pub fn spawn_operation<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.lane_handle(self.current_lane())
            .unwrap_or_else(|| panic!("no Tokio runtime is available for SDK work"))
            .spawn(future)
    }
}
fn is_dedicated(handle: &Handle) -> bool {
    matches!(DEDICATED.get(), Some(Ok(dedicated)) if dedicated.id() == handle.id())
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
                lanes: Mutex::new(HashMap::new()),
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
