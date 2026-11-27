//! Select the process-wide Tokio runtime on first use.
use crate::{operation::dedicated, BraintrustError, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tokio::runtime::{Handle, Id};

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
static SELECTED: OnceLock<std::result::Result<Handle, String>> = OnceLock::new();

fn select(config: &ServiceRuntime) -> std::result::Result<Handle, String> {
    match config {
        // Unit tests drive separate, short-lived runtimes in one process.
        ServiceRuntime::Current if cfg!(test) => dedicated(),
        ServiceRuntime::Current => Handle::try_current().or_else(|_| dedicated()),
        ServiceRuntime::Dedicated => dedicated(),
        ServiceRuntime::Handle(handle) => Ok(handle.clone()),
    }
}

static STOPPED: AtomicBool = AtomicBool::new(false);

struct RuntimeLifetime;

impl Drop for RuntimeLifetime {
    fn drop(&mut self) {
        STOPPED.store(true, Ordering::Release);
    }
}

pub(crate) fn mark_stopped(id: Id) {
    if SELECTED
        .get()
        .and_then(|runtime| runtime.as_ref().ok())
        .is_some_and(|runtime| runtime.id() == id)
    {
        STOPPED.store(true, Ordering::Release);
    }
}

pub(crate) fn handle() -> Result<Handle> {
    let selected = SELECTED.get_or_init(|| {
        let handle = select(&crate::service::config().runtime)?;
        // Construct the guard before spawning, so shutdown before the first
        // poll also marks this runtime stopped.
        let lifetime = RuntimeLifetime;
        handle.spawn(async move {
            let _lifetime = lifetime;
            std::future::pending::<()>().await;
        });
        Ok(handle)
    });
    if STOPPED.load(Ordering::Acquire) {
        dedicated().map_err(BraintrustError::Background)
    } else {
        selected.clone().map_err(BraintrustError::Background)
    }
}
