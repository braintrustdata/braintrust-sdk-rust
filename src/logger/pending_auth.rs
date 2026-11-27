//! Writes admitted by the client while waiting to learn their destination at login.
use crate::{
    BraintrustError, Completion, DeliveryError, DeliveryReceipt, EnqueueError, ExecutionError,
    FlushError, FlushFailure, FlushReport, WriteError,
};
use futures::{future::BoxFuture, future::Shared, FutureExt};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

type Write = Completion<DeliveryReceipt, WriteError>;
type Receipt = Result<FlushReport, FlushError>;
type AwaitReceipt = Shared<BoxFuture<'static, Receipt>>;

#[derive(Default)]
pub(super) struct PendingAuth {
    shared: Arc<Mutex<State>>,
    scope: Arc<Scope>,
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
    pending: BTreeMap<u64, (Arc<Scope>, AwaitReceipt)>,
}

pub(super) struct Reservation {
    scope: Arc<PendingAuth>,
    id: u64,
    sender: Option<oneshot::Sender<Receipt>>,
}

impl PendingAuth {
    pub(super) fn child(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            scope: Arc::new(Scope {
                parent: Some(self.scope.clone()),
                ..Scope::default()
            }),
        }
    }

    pub(super) fn reserve(self: &Arc<Self>) -> Result<Reservation, BraintrustError> {
        let mut state = self.shared.lock().unwrap();
        if self.scope.any(|scope| &scope.closed) {
            return Err(closed());
        }
        let (sender, receiver) = oneshot::channel();
        let id = state.next;
        state.next += 1;
        state.pending.insert(
            id,
            (
                self.scope.clone(),
                async move {
                    receiver
                        .await
                        .unwrap_or(Err(ExecutionError::WorkerLost.into()))
                }
                .boxed()
                .shared(),
            ),
        );
        Ok(Reservation {
            scope: self.clone(),
            id,
            sender: Some(sender),
        })
    }

    pub(super) fn direct<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let _state = self.shared.lock().unwrap();
        (!self.scope.any(|scope| &scope.closed)).then(f)
    }

    pub(super) fn abort(&self) {
        let _state = self.shared.lock().unwrap();
        self.scope.aborted.store(true, Ordering::Relaxed);
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
            .map(|(_, pending)| pending.clone())
            .collect();
        (callback(pending.is_empty()), pending)
    }
}

impl Reservation {
    /// Atomically move a reservation into the queue, so a flush covers it once.
    pub(super) fn publish(
        mut self,
        submit: impl FnOnce() -> Result<Vec<Write>, BraintrustError>,
    ) -> Result<(), BraintrustError> {
        let mut state = self.scope.shared.lock().unwrap();
        let result = if self.scope.scope.any(|scope| &scope.aborted) {
            Err(closed())
        } else {
            submit()
        };
        state.pending.remove(&self.id);
        drop(state);
        let sender = self.sender.take().expect("unpublished receipt");
        match result {
            Ok(writes) => {
                crate::operation::submit(async move {
                    let result = summarize(writes).await;
                    let _ = sender.send(result);
                    Ok::<_, ExecutionError>(())
                });
                Ok(())
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
        if self.sender.is_some() {
            self.scope.shared.lock().unwrap().pending.remove(&self.id);
        }
    }
}

pub(super) fn rejected<T, E>(error: E) -> Completion<T, E> {
    let (_, mut completion) = Completion::pair();
    completion.record_issuance(Err(error));
    completion
}

pub(super) fn closed() -> BraintrustError {
    BraintrustError::Background(EnqueueError::Closed.to_string())
}

async fn summarize(writes: Vec<Write>) -> Receipt {
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
    receipts
        .into_iter()
        .try_fold(barrier?, |mut report, receipt| {
            report.delivered += receipt?.delivered;
            Ok(report)
        })
}
