//! Observation of work submitted when an SDK method is called.
use crate::ExecutionError;
use std::{
    any::Any,
    future::Future,
    ops::Deref,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

/// Failure to wait synchronously, distinct from the operation's own result.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BlockingWaitError<E> {
    /// A pending wait was refused in an entered Tokio runtime context.
    #[error("cannot block in a Tokio runtime context; await the completion instead")]
    InRuntime,
    /// [`Completion::blocking_wait_timeout`] gave up. The operation keeps running.
    #[error("timed out waiting for the operation to complete")]
    TimedOut,
    /// Submission, execution or delivery failed.
    #[error(transparent)]
    Operation(#[from] E),
}

/// A synchronously recorded issuance result and an eventual operation result.
///
/// The operation starts when the SDK method is called, not when this is polled.
/// Inspect [`Completion::submission`] to check admission without waiting. Awaiting
/// returns that error, or the accepted operation's result. Dropping this token does
/// not cancel accepted work.
///
/// `Completion` is an [`Unpin`] [`Future`], so it can also be awaited through
/// `&mut` (for example with `tokio::time::timeout(duration, &mut completion)`)
/// and passed to combinators like `futures::future::join_all`. Once the result
/// has been returned, polling again returns [`ExecutionError::AlreadyTaken`].
/// If the operation panics, the panic resumes in whoever awaits or waits on it.
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
    /// The result has been returned by `poll`.
    taken: bool,
    /// Where a panicking operation leaves its payload, to re-raise for the waiter.
    panic: Option<PanicSlot>,
}
pub(crate) type PanicSlot = Arc<Mutex<Option<Box<dyn Any + Send>>>>;
/// The error for a sender dropped without a result: the operation's panic,
/// re-raised here, or a lost worker.
fn lost<E: From<ExecutionError>>(panic: &Option<PanicSlot>) -> E {
    if let Some(payload) = panic.as_ref().and_then(|slot| slot.lock().unwrap().take()) {
        std::panic::resume_unwind(payload);
    }
    ExecutionError::WorkerLost.into()
}
impl<T, E> Completion<T, E> {
    /// Borrow the immediate submission result without waiting for delivery.
    ///
    /// An accepted submission may still fail during delivery. Await this
    /// completion to observe the final result. After awaiting has returned a
    /// submission error, this returns [`ExecutionError::AlreadyTaken`] instead.
    pub fn submission(&self) -> &Result<(), E> {
        &self.issued
    }

    /// Wait for the operation's final result from synchronous code.
    ///
    /// Work already runs on the SDK's runtime (see [`crate::ServiceRuntime`]);
    /// this only waits on its response. Ready results can be read anywhere.
    /// Pending waits return [`BlockingWaitError::InRuntime`] whenever a Tokio
    /// runtime context is entered, including contexts where blocking might happen
    /// to work. Await instead there. Refusing a wait does not cancel accepted work.
    ///
    /// Outside Tokio this blocks until the result arrives. If the SDK runs on a
    /// current-thread runtime, another thread must be driving it, or this waits
    /// forever; [`Self::blocking_wait_timeout`] bounds the wait.
    pub fn blocking_wait(self) -> Result<T, BlockingWaitError<E>>
    where
        E: From<ExecutionError>,
    {
        let (receiver, panic) = match self.ready() {
            Ok(result) => return result,
            Err(pending) => pending,
        };
        receiver
            .blocking_recv()
            .unwrap_or_else(|_| Err(lost(&panic)))
            .map_err(BlockingWaitError::Operation)
    }

    /// Like [`Self::blocking_wait`], but gives up after `timeout` with
    /// [`BlockingWaitError::TimedOut`]. The operation keeps running.
    pub fn blocking_wait_timeout(self, timeout: Duration) -> Result<T, BlockingWaitError<E>>
    where
        E: From<ExecutionError>,
    {
        let (mut receiver, panic) = match self.ready() {
            Ok(result) => return result,
            Err(pending) => pending,
        };
        let deadline = Instant::now() + timeout;
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        loop {
            if let Poll::Ready(result) = Pin::new(&mut receiver).poll(&mut context) {
                return result
                    .unwrap_or_else(|_| Err(lost(&panic)))
                    .map_err(BlockingWaitError::Operation);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BlockingWaitError::TimedOut);
            }
            std::thread::park_timeout(remaining);
        }
    }

    /// The result if a wait needs no blocking, otherwise the receiver to block on.
    #[allow(clippy::type_complexity)]
    fn ready(
        mut self,
    ) -> Result<Result<T, BlockingWaitError<E>>, (oneshot::Receiver<Result<T, E>>, Option<PanicSlot>)>
    where
        E: From<ExecutionError>,
    {
        if self.taken {
            return Ok(Err(BlockingWaitError::Operation(
                ExecutionError::AlreadyTaken.into(),
            )));
        }
        if let Err(error) = self.issued {
            return Ok(Err(BlockingWaitError::Operation(error)));
        }
        match self.receiver.try_recv() {
            Ok(result) => return Ok(result.map_err(BlockingWaitError::Operation)),
            Err(oneshot::error::TryRecvError::Closed) => {
                return Ok(Err(BlockingWaitError::Operation(lost(&self.panic))));
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            return Ok(Err(BlockingWaitError::InRuntime));
        }
        Err((self.receiver, self.panic))
    }

    pub(crate) fn pair() -> (oneshot::Sender<Result<T, E>>, Self) {
        let (sender, receiver) = oneshot::channel();
        (
            sender,
            Self {
                issued: Ok(()),
                receiver,
                taken: false,
                panic: None,
            },
        )
    }
    /// A slot for the operation's panic, re-raised when this is awaited or waited on.
    pub(crate) fn panic_slot(&mut self) -> PanicSlot {
        self.panic.get_or_insert_with(Default::default).clone()
    }
    pub(crate) fn record_issuance(&mut self, result: Result<(), E>) {
        self.issued = result;
    }
}
/// Wakes a thread parked in [`Completion::blocking_wait_timeout`].
struct Unpark(std::thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
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
// No field is structurally pinned; the channel receiver is Unpin.
impl<T, E> Unpin for Completion<T, E> {}
impl<T, E: From<ExecutionError>> Future for Completion<T, E> {
    type Output = Result<T, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.taken {
            return Poll::Ready(Err(ExecutionError::AlreadyTaken.into()));
        }
        if this.issued.is_err() {
            this.taken = true;
            let issued =
                std::mem::replace(&mut this.issued, Err(ExecutionError::AlreadyTaken.into()));
            if let Err(error) = issued {
                return Poll::Ready(Err(error));
            }
        }
        let result = match Pin::new(&mut this.receiver).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(result)) => result,
            Poll::Ready(Err(_)) => {
                this.taken = true;
                Err(lost(&this.panic))
            }
        };
        // A completed receiver must not be polled again.
        this.taken = true;
        Poll::Ready(result)
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
    #[tokio::test]
    async fn awaiting_through_a_reference_keeps_the_completion_until_its_result_is_taken() {
        let (sender, mut completion) = Completion::<usize, ExecutionError>::pair();
        let timeout = std::time::Duration::from_millis(1);
        assert!(tokio::time::timeout(timeout, &mut completion)
            .await
            .is_err());
        assert!(completion.submission().is_ok());
        sender.send(Ok(42)).unwrap();
        assert_eq!((&mut completion).await, Ok(42));
        assert_eq!(completion.await, Err(ExecutionError::AlreadyTaken));

        let (_, mut rejected) = Completion::<(), ExecutionError>::pair();
        rejected.record_issuance(Err(ExecutionError::TimedOut));
        assert_eq!((&mut rejected).await, Err(ExecutionError::TimedOut));
        // The error was moved out; E need not be Clone.
        assert_eq!(rejected.submission(), &Err(ExecutionError::AlreadyTaken));
        assert_eq!(
            rejected.blocking_wait(),
            Err(BlockingWaitError::Operation(ExecutionError::AlreadyTaken))
        );
    }
    #[tokio::test]
    async fn completions_work_with_future_combinators() {
        let mut senders = Vec::new();
        let mut completions = Vec::new();
        for _ in 0..3 {
            let (sender, completion) = Completion::<usize, ExecutionError>::pair();
            senders.push(sender);
            completions.push(completion);
        }
        for (i, sender) in senders.into_iter().enumerate() {
            sender.send(Ok(i)).unwrap();
        }
        let results = futures::future::join_all(completions).await;
        assert_eq!(results, [Ok(0), Ok(1), Ok(2)]);
    }
    #[test]
    fn blocking_wait_timeout_gives_up_without_cancelling_the_operation() {
        let (sender, completion) = Completion::<usize, ExecutionError>::pair();
        let timeout = std::time::Duration::from_millis(20);
        assert_eq!(
            completion.blocking_wait_timeout(timeout),
            Err(BlockingWaitError::TimedOut)
        );
        drop(sender);

        let (sender, completion) = Completion::<usize, ExecutionError>::pair();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            sender.send(Ok(42)).unwrap();
        });
        assert_eq!(
            completion.blocking_wait_timeout(std::time::Duration::from_secs(5)),
            Ok(42)
        );
        worker.join().unwrap();
    }
    #[tokio::test]
    async fn an_operations_panic_reaches_whoever_awaits_it() {
        let completion = crate::service::submit::<(), ExecutionError>(async { panic!("boom") });
        let caught = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(completion));
        let payload = caught.await.unwrap_err();
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"boom"));
    }
    #[test]
    fn an_operations_panic_reaches_a_blocking_waiter() {
        let completion = crate::service::submit::<(), ExecutionError>(async { panic!("boom") });
        let payload =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| completion.blocking_wait()))
                .unwrap_err();
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"boom"));
    }
    #[test]
    fn blocking_wait_timeout_returns_the_operation_error_unchanged() {
        let later = |result: Option<Result<(), ExecutionError>>| {
            let (sender, completion) = Completion::<(), ExecutionError>::pair();
            let worker = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10));
                match result {
                    Some(result) => sender.send(result).unwrap(),
                    None => drop(sender),
                }
            });
            let waited = completion.blocking_wait_timeout(std::time::Duration::from_secs(5));
            worker.join().unwrap();
            waited
        };
        // The operation's own error arrives as sent.
        assert_eq!(
            later(Some(Err(ExecutionError::TimedOut))),
            Err(BlockingWaitError::Operation(ExecutionError::TimedOut))
        );
        // Only a sender dropped without a result, which carries no error, is WorkerLost.
        assert_eq!(
            later(None),
            Err(BlockingWaitError::Operation(ExecutionError::WorkerLost))
        );
    }
    #[tokio::test]
    async fn pending_blocking_wait_timeout_in_async_context_returns_an_error() {
        let (_sender, completion) = Completion::<(), ExecutionError>::pair();
        assert_eq!(
            completion.blocking_wait_timeout(std::time::Duration::from_secs(5)),
            Err(BlockingWaitError::InRuntime)
        );
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
