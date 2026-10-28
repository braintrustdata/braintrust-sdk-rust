//! A bounded final outcome for repeated observers of one logical operation.
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::oneshot;

use crate::{Completion, EnqueueError, ExecutionError};

type Sender<T, E> = oneshot::Sender<Result<T, E>>;
const MAX_PENDING_OBSERVERS: usize = 1024;

/// Lives with its operation owner, not with an active-work registry.
pub(crate) struct SharedOutcome<T, E>(Arc<Mutex<State<T, E>>>);
struct State<T, E> {
    result: Option<Result<T, E>>,
    observers: Vec<Sender<T, E>>,
}
impl<T, E> Clone for SharedOutcome<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T, E> Default for SharedOutcome<T, E> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            result: None,
            observers: Vec::new(),
        })))
    }
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl<T: Clone, E: Clone + From<EnqueueError>> SharedOutcome<T, E> {
    pub fn observe(&self) -> Completion<T, E> {
        let (sender, mut completion) = Completion::pair();
        let mut state = lock(&self.0);
        if let Some(result) = &state.result {
            let result = result.clone();
            drop(state);
            let _ = sender.send(result);
        } else {
            state.observers.retain(|observer| !observer.is_closed());
            if state.observers.len() >= MAX_PENDING_OBSERVERS {
                completion.record_issuance(Err(EnqueueError::Full.into()));
            } else {
                state.observers.push(sender);
            }
        }
        completion
    }

    pub fn finish(&self, result: Result<T, E>) {
        let mut state = lock(&self.0);
        if state.result.is_some() {
            return;
        }
        let observers = std::mem::take(&mut state.observers);
        state.result = Some(result.clone());
        drop(state);
        for sender in observers {
            let _ = sender.send(result.clone());
        }
    }
}

/// Publish to both a one-shot observer and an optional shared final outcome.
/// No relay task or polling of a completion token is needed.
pub(crate) struct SharedResponder<T: Clone, E: Clone + From<EnqueueError> + From<ExecutionError>> {
    sender: Option<Sender<T, E>>,
    outcome: Option<SharedOutcome<T, E>>,
}
impl<T: Clone, E: Clone + From<EnqueueError> + From<ExecutionError>> SharedResponder<T, E> {
    pub fn new(sender: Sender<T, E>, outcome: Option<SharedOutcome<T, E>>) -> Self {
        Self {
            sender: Some(sender),
            outcome,
        }
    }
    pub fn send(mut self, result: Result<T, E>) -> Result<(), Result<T, E>> {
        if let Some(outcome) = self.outcome.take() {
            outcome.finish(result.clone());
        }
        match self.sender.take() {
            Some(sender) => sender.send(result),
            None => Err(result),
        }
    }
}
impl<T: Clone, E: Clone + From<EnqueueError> + From<ExecutionError>> Drop
    for SharedResponder<T, E>
{
    fn drop(&mut self) {
        if let Some(outcome) = self.outcome.take() {
            outcome.finish(Err(ExecutionError::WorkerLost.into()));
        }
    }
}

pub(crate) type WriteOutcome = SharedOutcome<crate::DeliveryReceipt, crate::WriteError>;
pub(crate) type WriteResponder = SharedResponder<crate::DeliveryReceipt, crate::WriteError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeliveryReceipt, WriteError};

    #[test]
    fn publication_wakes_observers_outside_the_outcome_lock() {
        use std::future::{Future, IntoFuture};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::Context;

        struct Reenter {
            outcome: WriteOutcome,
            wakes: AtomicUsize,
        }
        impl futures::task::ArcWake for Reenter {
            fn wake_by_ref(this: &Arc<Self>) {
                assert!(this.outcome.observe().is_ok());
                this.wakes.fetch_add(1, Ordering::SeqCst);
            }
        }
        let outcome = WriteOutcome::default();
        let observer = Arc::new(Reenter {
            outcome: outcome.clone(),
            wakes: AtomicUsize::new(0),
        });
        let waker = futures::task::waker(observer.clone());
        let mut context = Context::from_waker(&waker);
        let mut completion = Box::pin(outcome.observe().into_future());
        assert!(completion.as_mut().poll(&mut context).is_pending());
        outcome.finish(Ok(DeliveryReceipt));
        assert_eq!(observer.wakes.load(Ordering::SeqCst), 1);
        assert!(completion.as_mut().poll(&mut context).is_ready());
    }

    #[tokio::test]
    async fn pending_observers_are_bounded_and_dropped_tokens_release_slots() {
        let outcome = WriteOutcome::default();
        let mut observers: Vec<_> = (0..MAX_PENDING_OBSERVERS)
            .map(|_| outcome.observe())
            .collect();
        let full = outcome.observe();
        assert!(matches!(
            &*full,
            Err(WriteError::Admission(EnqueueError::Full))
        ));
        assert!(matches!(
            full.await,
            Err(WriteError::Admission(EnqueueError::Full))
        ));
        drop(observers.pop());
        let replacement = outcome.observe();
        assert!(replacement.is_ok());
        outcome.finish(Ok(DeliveryReceipt));
        for observer in observers {
            observer.await.unwrap();
        }
        replacement.await.unwrap();
        outcome.observe().await.unwrap();
    }

    #[tokio::test]
    async fn producer_loss_completes_present_and_future_observers_without_a_relay_task() {
        let outcome = WriteOutcome::default();
        let before = outcome.observe();
        let (sender, initial) = Completion::pair();
        drop(WriteResponder::new(sender, Some(outcome.clone())));
        assert!(matches!(
            initial.await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
        assert!(matches!(
            before.await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
        assert!(matches!(
            outcome.observe().await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
    }
}
