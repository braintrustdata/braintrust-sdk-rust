//! One final write outcome shared by repeated span-end observers.
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::oneshot;

use crate::{Completion, DeliveryReceipt, EnqueueError, ExecutionError, WriteError};

type Outcome = Result<DeliveryReceipt, WriteError>;
type Sender = oneshot::Sender<Outcome>;
const MAX_PENDING_OBSERVERS: usize = 1024;

/// Lives with a finished span, not with the queue. Holds no command or payload.
#[derive(Clone, Default)]
pub(crate) struct WriteOutcome(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    result: Option<Outcome>,
    observers: Vec<Sender>,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl WriteOutcome {
    pub fn observe(&self) -> Completion<DeliveryReceipt, WriteError> {
        let (sender, mut completion) = Completion::pair();
        let mut state = lock(&self.0);
        if let Some(result) = &state.result {
            let _ = sender.send(result.clone());
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

    fn finish(&self, result: Outcome) {
        let mut state = lock(&self.0);
        if state.result.is_some() {
            return;
        }
        for sender in state.observers.drain(..) {
            let _ = sender.send(result.clone());
        }
        state.result = Some(result);
    }
}

/// Connect the existing batch sender to a span's optional shared final outcome.
/// No relay task or polling of a completion token is needed.
pub(crate) struct WriteResponder {
    sender: Option<Sender>,
    outcome: Option<WriteOutcome>,
}
impl WriteResponder {
    pub fn new(sender: Sender, outcome: Option<WriteOutcome>) -> Self {
        Self {
            sender: Some(sender),
            outcome,
        }
    }
    pub fn send(mut self, result: Outcome) -> Result<(), Outcome> {
        if let Some(outcome) = self.outcome.take() {
            outcome.finish(result.clone());
        }
        match self.sender.take() {
            Some(sender) => sender.send(result),
            None => Err(result),
        }
    }
}
impl Drop for WriteResponder {
    fn drop(&mut self) {
        if let Some(outcome) = self.outcome.take() {
            outcome.finish(Err(ExecutionError::WorkerLost.into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
