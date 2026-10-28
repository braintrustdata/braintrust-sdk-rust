//! Explicit selection of a caller-owned Tokio runtime.
/// Capture the current Tokio runtime, or `None` outside Tokio.
///
/// Pass this to [`crate::BraintrustClientBuilder::runtime`] to explicitly opt
/// into the caller's runtime. The default `None` always uses an SDK-owned runtime,
/// including when construction happens inside Tokio. With an explicit handle,
/// callers must keep that runtime alive and driving accepted work.
pub fn current_runtime() -> Option<tokio::runtime::Handle> {
    tokio::runtime::Handle::try_current().ok()
}
