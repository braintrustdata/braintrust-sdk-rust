//! Observation of work submitted when an SDK method is called.
use crate::ExecutionError;
use std::{
    future::{Future, IntoFuture},
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::oneshot;

/// Failure to wait synchronously, distinct from the operation's own result.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BlockingWaitError<E> {
    /// A pending wait was refused in an entered Tokio runtime context.
    #[error("cannot block in a Tokio runtime context; await the completion instead")]
    InRuntime,
    /// Submission, execution or delivery failed.
    #[error(transparent)]
    Operation(#[from] E),
}

/// A synchronously recorded issuance result and an eventual operation result.
///
/// Inspect [`Completion::submission`] to check admission without waiting. Awaiting returns
/// that error, or the accepted operation's result. Dropping this token does not
/// cancel accepted work. No work is started by polling the token.
///
/// ```no_run
/// use braintrust_sdk_rust::{BraintrustClient, SpanHandle, SpanLog};
/// use std::time::Duration;
///
/// # async fn example(client: &BraintrustClient, span: &SpanHandle<BraintrustClient>)
/// # -> Result<(), Box<dyn std::error::Error>> {
/// let completion = span.log(SpanLog::default()); // admission happens now
/// if let Err(error) = completion.submission() {
///     eprintln!("submission rejected: {error}");
/// }
/// completion.await?; // observes admission failure or full batch delivery
/// let report = client.flush().await?;
/// client.shutdown(Duration::from_secs(10)).await?;
/// # Ok(())
/// # }
/// ```
pub struct Completion<T, E> {
    issued: Result<(), E>,
    receiver: oneshot::Receiver<Result<T, E>>,
    cancellation: Option<tokio::task::AbortHandle>,
}
impl<T, E> Completion<T, E> {
    /// Return an already completed operation, useful for synchronous trait implementations.
    pub fn ready(result: Result<T, E>) -> Self {
        crate::service::ready(result)
    }

    /// Start owned asynchronous work on the shared SDK runtime immediately.
    /// Dropping the returned completion does not cancel the operation.
    pub fn spawn(operation: impl Future<Output = Result<T, E>> + Send + 'static) -> Self
    where
        T: Send + 'static,
        E: From<ExecutionError> + Send + 'static,
    {
        crate::service::submit(operation)
    }

    /// Borrow the immediate submission result without waiting for delivery.
    ///
    /// An accepted submission may still fail during delivery. Await this
    /// completion to observe the final result.
    pub fn submission(&self) -> &Result<(), E> {
        &self.issued
    }

    /// Wait for the operation's final result from synchronous code.
    ///
    /// Work already runs on the SDK runtime; this only waits on its response.
    /// Ready results can be read anywhere. Pending waits return
    /// [`BlockingWaitError::InRuntime`] whenever a Tokio runtime context is entered,
    /// including contexts where blocking might happen to work. Await instead there.
    /// Refusing a wait does not cancel accepted work.
    ///
    /// Outside Tokio, the SDK-owned runtime progresses independently.
    pub fn blocking_wait(mut self) -> Result<T, BlockingWaitError<E>>
    where
        E: From<ExecutionError>,
    {
        self.issued.map_err(BlockingWaitError::Operation)?;
        match self.receiver.try_recv() {
            Ok(result) => return result.map_err(BlockingWaitError::Operation),
            Err(oneshot::error::TryRecvError::Closed) => {
                return Err(BlockingWaitError::Operation(
                    ExecutionError::WorkerLost.into(),
                ));
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(BlockingWaitError::InRuntime);
        }
        self.receiver
            .blocking_recv()
            .unwrap_or_else(|_| Err(ExecutionError::WorkerLost.into()))
            .map_err(BlockingWaitError::Operation)
    }

    pub(crate) fn pair() -> (oneshot::Sender<Result<T, E>>, Self) {
        let (sender, receiver) = oneshot::channel();
        (
            sender,
            Self {
                issued: Ok(()),
                receiver,
                cancellation: None,
            },
        )
    }
    pub(crate) fn set_cancellation(&mut self, handle: tokio::task::AbortHandle) {
        self.cancellation = Some(handle);
    }

    /// Internal execution deadlines may cancel work; dropping a public observer never does.
    pub(crate) fn cancel_on_drop(mut self) -> impl Future<Output = Result<T, E>>
    where
        E: From<ExecutionError>,
    {
        struct Cancel(Option<tokio::task::AbortHandle>);
        impl Drop for Cancel {
            fn drop(&mut self) {
                if let Some(handle) = &self.0 {
                    handle.abort();
                }
            }
        }
        let guard = Cancel(self.cancellation.take());
        async move {
            let _guard = guard;
            self.await
        }
    }

    pub(crate) fn record_issuance(&mut self, result: Result<(), E>) {
        self.issued = result;
    }
}
impl<T, E> Deref for Completion<T, E> {
    type Target = Result<(), E>;
    fn deref(&self) -> &Self::Target {
        &self.issued
    }
}
impl<T, E> std::fmt::Debug for Completion<T, E>
where
    E: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Completion")
            .field("issued", &self.issued)
            .finish_non_exhaustive()
    }
}
/// The future produced by consuming a [`Completion`].
pub struct CompletionFuture<T, E> {
    issued: Option<Result<(), E>>,
    receiver: oneshot::Receiver<Result<T, E>>,
}
// Neither field is structurally pinned; the channel receiver is Unpin.
impl<T, E> Unpin for CompletionFuture<T, E> {}
impl<T, E: From<ExecutionError>> IntoFuture for Completion<T, E> {
    type Output = Result<T, E>;
    type IntoFuture = CompletionFuture<T, E>;
    fn into_future(self) -> Self::IntoFuture {
        CompletionFuture {
            issued: Some(self.issued),
            receiver: self.receiver,
        }
    }
}
impl<T, E: From<ExecutionError>> Future for CompletionFuture<T, E> {
    type Output = Result<T, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(Err(error)) = this.issued.take() {
            return Poll::Ready(Err(error));
        }
        match Pin::new(&mut this.receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(ExecutionError::WorkerLost.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn issuance_error_needs_neither_clone_nor_producer() {
        #[derive(Debug, PartialEq)]
        enum Error {
            Admission,
            Execution,
        }
        impl From<ExecutionError> for Error {
            fn from(_: ExecutionError) -> Self {
                Self::Execution
            }
        }
        let (sender, mut token) = Completion::<(), Error>::pair();
        token.record_issuance(Err(Error::Admission));
        assert_eq!(token.submission(), &Err(Error::Admission));
        assert_eq!(&*token, token.submission());
        drop(sender);
        assert_eq!(token.await, Err(Error::Admission));
    }
    #[tokio::test]
    async fn successful_issuance_stays_successful_after_operation_failure() {
        let (sender, token) = Completion::<(), ExecutionError>::pair();
        sender.send(Err(ExecutionError::TimedOut)).unwrap();
        assert!(token.submission().is_ok());
        assert!(token.is_ok());
        assert_eq!(token.await, Err(ExecutionError::TimedOut));
        let (sender, token) = Completion::<(), ExecutionError>::pair();
        drop(sender);
        assert_eq!(token.await, Err(ExecutionError::WorkerLost));
    }
    #[test]
    fn blocking_wait_observes_the_background_response_without_a_runtime() {
        #[derive(Debug)]
        struct Value(usize);
        let (sender, completion) = Completion::<Value, ExecutionError>::pair();
        let worker = std::thread::spawn(move || sender.send(Ok(Value(42))).unwrap());
        assert_eq!(completion.blocking_wait().unwrap().0, 42);
        worker.join().unwrap();
        let (sender, completion) = Completion::<(), ExecutionError>::pair();
        drop(sender);
        assert_eq!(
            completion.blocking_wait(),
            Err(BlockingWaitError::Operation(ExecutionError::WorkerLost))
        );
        let (_, mut rejected) = Completion::<(), ExecutionError>::pair();
        rejected.record_issuance(Err(ExecutionError::TimedOut));
        assert_eq!(
            rejected.blocking_wait(),
            Err(BlockingWaitError::Operation(ExecutionError::TimedOut))
        );
    }
    #[tokio::test]
    async fn pending_blocking_wait_in_async_context_returns_an_error() {
        let (_sender, completion) = Completion::<(), ExecutionError>::pair();
        assert_eq!(
            completion.blocking_wait(),
            Err(BlockingWaitError::InRuntime)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_wait_is_also_refused_on_a_multi_thread_runtime() {
        let (_sender, completion) = Completion::<(), ExecutionError>::pair();
        assert_eq!(
            completion.blocking_wait(),
            Err(BlockingWaitError::InRuntime)
        );
    }

    #[tokio::test]
    async fn ready_results_and_admission_failures_need_no_blocking() {
        for result in [Ok(42), Err(ExecutionError::TimedOut)] {
            let (sender, completion) = Completion::pair();
            sender.send(result).unwrap();
            assert_eq!(
                completion.blocking_wait(),
                result.map_err(BlockingWaitError::Operation)
            );
        }
        let (_, mut rejected) = Completion::<(), ExecutionError>::pair();
        rejected.record_issuance(Err(ExecutionError::TimedOut));
        assert_eq!(
            rejected.blocking_wait(),
            Err(BlockingWaitError::Operation(ExecutionError::TimedOut))
        );
    }

    #[test]
    fn pending_wait_in_an_entered_handle_is_conservatively_refused() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let (_sender, completion) = Completion::<(), ExecutionError>::pair();
        assert_eq!(
            completion.blocking_wait(),
            Err(BlockingWaitError::InRuntime)
        );
    }
    #[test]
    fn external_current_thread_work_still_requires_its_runtime_driver() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (sender, completion) = Completion::<usize, ExecutionError>::pair();
        let operation = runtime.spawn(async move { sender.send(Ok(42)).unwrap() });
        let (finished, result) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || finished.send(completion.blocking_wait()).unwrap());
        assert!(matches!(
            result.recv_timeout(std::time::Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        runtime.block_on(operation).unwrap();
        assert_eq!(
            result
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            42
        );
        caller.join().unwrap();
    }
}
