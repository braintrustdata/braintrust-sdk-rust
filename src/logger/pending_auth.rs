//! Writes admitted by the client while waiting to learn their destination at login.
use crate::{
    BraintrustError, Completion, DeliveryError, DeliveryReceipt, EnqueueError, ExecutionError,
    FlushError, FlushFailure, FlushReport, WriteError,
};
use futures::{future::BoxFuture, future::Shared, FutureExt};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Notify};

type Write = Completion<DeliveryReceipt, WriteError>;
type Receipt = Result<FlushReport, FlushError>;
type AwaitReceipt = Shared<BoxFuture<'static, Receipt>>;

struct PendingReceipt {
    receipt: AwaitReceipt,
    sender: oneshot::Sender<Receipt>,
}

#[derive(Default)]
pub(super) struct PendingAuth {
    shared: Arc<Mutex<State>>,
    scope: Arc<Scope>,
    cancelled: Arc<Notify>,
    draining: AtomicBool,
}

#[derive(Default)]
struct Scope {
    parent: Option<Arc<Scope>>,
    closed: AtomicBool,
    aborted: AtomicBool,
}

impl Scope {
    fn includes(&self, ancestor: &Self) -> bool {
        std::ptr::eq(self, ancestor)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.includes(ancestor))
    }

    fn any(&self, predicate: fn(&Scope) -> &AtomicBool) -> bool {
        predicate(self).load(Ordering::Relaxed)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.any(predicate))
    }
}

#[derive(Default)]
struct State {
    next: u64,
    pending: BTreeMap<u64, (Arc<Scope>, PendingReceipt)>,
}

pub(crate) struct Reservation {
    scope: Arc<PendingAuth>,
    id: u64,
    capacity: Option<crate::log_queue::engine::PendingCapacity>,
}

pub(super) struct Drain(Arc<PendingAuth>);

impl Drop for Drain {
    fn drop(&mut self) {
        self.0.draining.store(false, Ordering::Release);
    }
}

impl PendingAuth {
    pub(super) fn child(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            cancelled: self.cancelled.clone(),
            draining: AtomicBool::new(false),
            scope: Arc::new(Scope {
                parent: Some(self.scope.clone()),
                ..Scope::default()
            }),
        }
    }

    pub(super) fn try_drain(self: &Arc<Self>) -> Option<Drain> {
        self.draining
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Drain(self.clone()))
    }

    pub(super) fn reserve(self: &Arc<Self>) -> Result<Reservation, BraintrustError> {
        let mut state = self.shared.lock().unwrap();
        if self.scope.any(|scope| &scope.closed) {
            return Err(closed());
        }
        let (sender, receiver) = oneshot::channel();
        let id = state.next;
        state.next += 1;
        let pending = PendingReceipt {
            receipt: async move {
                receiver
                    .await
                    .unwrap_or(Err(ExecutionError::WorkerLost.into()))
            }
            .boxed()
            .shared(),
            sender,
        };
        state.pending.insert(id, (self.scope.clone(), pending));
        Ok(Reservation {
            scope: self.clone(),
            id,
            capacity: None,
        })
    }

    pub(super) fn direct<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let _state = self.shared.lock().unwrap();
        (!self.scope.any(|scope| &scope.closed)).then(f)
    }

    pub(super) fn abort(&self) {
        let pending = {
            let mut state = self.shared.lock().unwrap();
            self.scope.aborted.store(true, Ordering::Relaxed);
            let expired: Vec<_> = state
                .pending
                .iter()
                .filter(|(_, (scope, _))| scope.includes(&self.scope))
                .map(|(&id, _)| id)
                .collect();
            expired
                .into_iter()
                .filter_map(|id| state.pending.remove(&id).map(|(_, pending)| pending))
                .collect::<Vec<_>>()
        };
        for pending in pending {
            let _ = pending.sender.send(Err(ExecutionError::TimedOut.into()));
        }
        self.cancelled.notify_waiters();
    }

    async fn aborted(&self) {
        loop {
            let notified = self.cancelled.notified();
            if self.scope.any(|scope| &scope.aborted) {
                return;
            }
            notified.await;
        }
    }

    /// Capture synchronous queue work and unpublished writes under one lock.
    pub(super) fn snapshot<T>(
        &self,
        close: bool,
        callback: impl FnOnce(bool) -> T,
    ) -> (T, Vec<AwaitReceipt>) {
        let state = self.shared.lock().unwrap();
        if close {
            self.scope.closed.store(true, Ordering::Relaxed);
        }
        let pending: Vec<_> = state
            .pending
            .values()
            .filter(|(scope, _)| scope.includes(&self.scope))
            .map(|(_, pending)| pending.receipt.clone())
            .collect();
        (callback(pending.is_empty()), pending)
    }
}

impl Reservation {
    pub(crate) async fn wait<T>(
        &self,
        prepare: impl std::future::Future<Output = Result<T, BraintrustError>>,
    ) -> Result<T, BraintrustError> {
        tokio::select! {
            biased;
            _ = self.scope.aborted() => Err(ExecutionError::TimedOut.into()),
            result = prepare => result,
        }
    }

    pub(super) fn reject(self, error: EnqueueError) {
        let pending = self.scope.shared.lock().unwrap().pending.remove(&self.id);
        if let Some((_, pending)) = pending {
            let _ = pending.sender.send(Err(error.into()));
        }
        self.scope.cancelled.notify_waiters();
    }

    pub(crate) fn set_capacity(&mut self, capacity: crate::log_queue::engine::PendingCapacity) {
        self.capacity = Some(capacity);
    }

    pub(crate) async fn wait_turn(&self) -> Result<(), BraintrustError> {
        loop {
            let notified = self.scope.cancelled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.scope.shared.lock().unwrap();
                if !state.pending.contains_key(&self.id) {
                    return Err(ExecutionError::TimedOut.into());
                }
                let earlier = state
                    .pending
                    .range(..self.id)
                    .any(|(_, (scope, _))| Arc::ptr_eq(scope, &self.scope.scope));
                if !earlier {
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    /// Atomically move a reservation into the queue, so a flush covers it once.
    pub(crate) fn publish(
        mut self,
        submit: impl FnOnce(
            Option<&mut crate::log_queue::engine::PendingCapacity>,
        ) -> Result<Vec<Write>, BraintrustError>,
    ) -> Result<(), BraintrustError> {
        let mut state = self.scope.shared.lock().unwrap();
        let Some(pending) = state.pending.remove(&self.id).map(|(_, pending)| pending) else {
            return Err(ExecutionError::TimedOut.into());
        };
        let result = submit(self.capacity.as_mut());
        drop(state);
        self.scope.cancelled.notify_waiters();
        let sender = pending.sender;
        match result {
            Ok(writes) => {
                let rejected = writes
                    .iter()
                    .find_map(|write| write.submission().as_ref().err().cloned());
                crate::operation::submit(async move {
                    let result = summarize(writes).await;
                    let _ = sender.send(result);
                    Ok::<_, ExecutionError>(())
                });
                rejected.map_or(Ok(()), |error| {
                    Err(BraintrustError::Background(error.to_string()))
                })
            }
            Err(error) => {
                let failure = match &error {
                    BraintrustError::Api { status, message } => BraintrustError::Api {
                        status: *status,
                        message: message.clone(),
                    }
                    .into(),
                    error => anyhow::anyhow!(error.to_string()),
                };
                let _ = sender.send(Err(FlushError::Delivery(FlushFailure {
                    failed: 1,
                    cause: Arc::new(DeliveryError { source: failure }),
                })));
                Err(error)
            }
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.scope.shared.lock().unwrap().pending.remove(&self.id);
        self.scope.cancelled.notify_waiters();
    }
}

pub(crate) fn rejected<T, E>(error: E) -> Completion<T, E> {
    let (_, mut completion) = Completion::pair();
    completion.record_issuance(Err(error));
    completion
}

pub(super) fn closed() -> BraintrustError {
    BraintrustError::Background(EnqueueError::Closed.to_string())
}

pub(crate) async fn summarize(writes: Vec<Write>) -> Receipt {
    let results = futures::future::join_all(writes).await;
    let mut delivered = 0;
    let mut failed = 0;
    let mut cause = None;
    for result in results {
        match result {
            Ok(_) => delivered += 1,
            Err(WriteError::Admission(error)) => return Err(error.into()),
            Err(WriteError::Execution(error)) => return Err(error.into()),
            Err(WriteError::Delivery(error)) => {
                failed += 1;
                cause.get_or_insert(error);
            }
        }
    }
    match cause {
        Some(cause) => Err(FlushError::Delivery(FlushFailure { failed, cause })),
        None => Ok(FlushReport { delivered }),
    }
}

pub(super) async fn join(
    barrier: Completion<FlushReport, FlushError>,
    pending: Vec<AwaitReceipt>,
) -> Receipt {
    let (barrier, receipts) = futures::join!(barrier, futures::future::join_all(pending));
    let mut delivered = 0;
    let mut failure: Option<FlushFailure> = None;
    for result in std::iter::once(barrier).chain(receipts) {
        match result {
            Ok(report) => delivered += report.delivered,
            Err(FlushError::Delivery(next)) => match &mut failure {
                Some(failure) => failure.failed += next.failed,
                None => failure = Some(next),
            },
            Err(error) => return Err(error),
        }
    }
    match failure {
        Some(failure) => Err(FlushError::Delivery(failure)),
        None => Ok(FlushReport { delivered }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejected_reservation_is_reported_to_existing_snapshot() {
        let pending = Arc::new(PendingAuth::default());
        let reservation = pending.reserve().unwrap();
        let (barrier, receipts) =
            pending.snapshot(false, |_| Completion::finished(Ok(FlushReport::default())));
        reservation.reject(EnqueueError::Full);
        assert!(matches!(
            join(barrier, receipts).await,
            Err(FlushError::Admission(EnqueueError::Full))
        ));
    }

    #[tokio::test]
    async fn rejected_published_write_reaches_operation_and_snapshot() {
        let pending = Arc::new(PendingAuth::default());
        let reservation = pending.reserve().unwrap();
        let (barrier, receipts) =
            pending.snapshot(false, |_| Completion::finished(Ok(FlushReport::default())));
        let (_, mut write) = Completion::pair();
        write.record_issuance(Err(WriteError::Admission(EnqueueError::Full)));
        assert!(reservation.publish(|_| Ok(vec![write])).is_err());
        assert!(matches!(
            join(barrier, receipts).await,
            Err(FlushError::Admission(EnqueueError::Full))
        ));
    }

    #[tokio::test]
    async fn later_reservation_waits_for_earlier_publication() {
        let pending = Arc::new(PendingAuth::default());
        let first = pending.reserve().unwrap();
        let second = pending.reserve().unwrap();
        let mut waiting = Box::pin(second.wait_turn());
        assert!(futures::poll!(&mut waiting).is_pending());
        first.publish(|_| Ok(Vec::new())).unwrap();
        waiting.await.unwrap();
        second.publish(|_| Ok(Vec::new())).unwrap();
    }
}
