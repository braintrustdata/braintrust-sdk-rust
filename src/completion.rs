//! Observation of work submitted when an SDK method is called.
use crate::ExecutionError;
use std::{
    future::{Future, IntoFuture},
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::oneshot;

/// A synchronously recorded issuance result and an eventual operation result.
///
/// Inspect `&*completion` to check admission without waiting. Awaiting returns
/// that error, or the accepted operation's result. Dropping this token does not
/// cancel accepted work. No work is started by polling the token.
///
/// ```no_run
/// use braintrust_sdk_rust::{BraintrustClient, SpanHandle, SpanLog};
/// use std::time::Duration;
///
/// # async fn example(client: &BraintrustClient, span: &SpanHandle)
/// # -> Result<(), Box<dyn std::error::Error>> {
/// let completion = span.log(SpanLog::default()); // admission happens now
/// if let Err(error) = &*completion {
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
}
impl<T, E> Completion<T, E> {
    pub(crate) fn pair() -> (oneshot::Sender<Result<T, E>>, Self) {
        let (sender, receiver) = oneshot::channel();
        (
            sender,
            Self {
                issued: Ok(()),
                receiver,
            },
        )
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
        assert_eq!(&*token, &Err(Error::Admission));
        drop(sender);
        assert_eq!(token.await, Err(Error::Admission));
    }
    #[tokio::test]
    async fn successful_issuance_stays_successful_after_operation_failure() {
        let (sender, token) = Completion::<(), ExecutionError>::pair();
        sender.send(Err(ExecutionError::TimedOut)).unwrap();
        assert!(token.is_ok());
        assert_eq!(token.await, Err(ExecutionError::TimedOut));
        let (sender, token) = Completion::<(), ExecutionError>::pair();
        drop(sender);
        assert_eq!(token.await, Err(ExecutionError::WorkerLost));
    }
}
