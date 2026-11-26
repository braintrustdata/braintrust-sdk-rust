//! Dispatch eager SDK operations and deliver their results to completion waiters.
use crate::{Completion, ExecutionError};
use futures::FutureExt;
use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::Handle;

/// Start an SDK-owned runtime for callers outside Tokio.
pub(crate) fn dedicated() -> Result<Handle, String> {
    static DEDICATED: OnceLock<Result<Handle, String>> = OnceLock::new();
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
                .map_err(|error| error.to_string())?;
            receiver.recv().map_err(|error| error.to_string())?
        })
        .clone()
}

pub(crate) fn submit<T, E>(
    operation: impl Future<Output = Result<T, E>> + Send + 'static,
) -> Completion<T, E>
where
    T: Send + 'static,
    E: From<ExecutionError> + Send + 'static,
{
    let (sender, mut completion) = Completion::pair();
    match crate::service::global()
        .ok()
        .and_then(crate::service::Service::runtime_handle)
    {
        Some(runtime) => {
            let panic = completion.panic_slot();
            runtime.spawn(async move {
                match std::panic::AssertUnwindSafe(operation).catch_unwind().await {
                    Ok(result) => {
                        let _ = sender.send(result);
                    }
                    // Store the payload before closing the channel so the waiter can re-raise it.
                    Err(payload) => *panic.lock().unwrap() = Some(payload),
                }
            });
        }
        None => completion.record_issuance(Err(ExecutionError::WorkerLost.into())),
    }
    completion
}
