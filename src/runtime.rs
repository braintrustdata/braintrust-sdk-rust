//! Select the process-wide Tokio runtime on first use.
use crate::{operation::dedicated, BraintrustError, Result};
use std::sync::OnceLock;
use tokio::runtime::Handle;

/// Where the SDK runs its work.
///
/// The SDK selects one runtime when the process first uses the SDK.
/// The selected runtime must have I/O and timers enabled. Keep driving a
/// current-thread runtime until its SDK work finishes.
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

pub(crate) fn handle() -> Result<Handle> {
    SELECTED
        .get_or_init(|| select(&crate::service::config().runtime))
        .clone()
        .map_err(BraintrustError::Background)
}
