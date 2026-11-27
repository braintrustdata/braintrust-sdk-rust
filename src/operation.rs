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
    match crate::runtime::handle() {
        Ok(runtime) => {
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
        Err(_) => completion.record_issuance(Err(ExecutionError::WorkerLost.into())),
    }
    completion
}

type OperationDone = futures::future::Shared<futures::future::BoxFuture<'static, ()>>;

/// Reserve operation order at the call site before scheduling work.
#[derive(Default)]
pub(crate) struct Sequence {
    tail: std::sync::Mutex<Option<OperationDone>>,
}

impl Sequence {
    pub(crate) fn submit<T, E>(
        &self,
        operation: impl Future<Output = Result<T, E>> + Send + 'static,
    ) -> Completion<T, E>
    where
        T: Send + 'static,
        E: From<ExecutionError> + Send + 'static,
    {
        let mut tail = self.tail.lock().unwrap();
        let previous = tail.take().filter(|done| done.peek().is_none());
        let wait = previous.clone();
        let (done, finished) = tokio::sync::oneshot::channel::<()>();
        *tail = Some(
            async move {
                if let Some(previous) = previous {
                    previous.await;
                }
                let _ = finished.await;
            }
            .boxed()
            .shared(),
        );
        submit(async move {
            let _done = done;
            if let Some(wait) = wait {
                wait.await;
            }
            operation.await
        })
    }
}

#[cfg(test)]
mod sequence_tests {
    use super::*;

    #[tokio::test]
    async fn eager_calls_run_in_reservation_order() {
        let sequence = Sequence::default();
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        let first = sequence.submit(async move {
            receive.await.unwrap();
            Ok::<_, ExecutionError>(())
        });
        let mut second = sequence.submit(async { Ok::<_, ExecutionError>(()) });
        tokio::task::yield_now().await;
        assert!(futures::poll!(&mut second).is_pending());
        send.send(()).unwrap();
        second.await.unwrap();
        first.await.unwrap();
    }
}
