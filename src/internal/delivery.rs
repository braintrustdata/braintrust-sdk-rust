//! Atomic admission, fixed flush cutoffs and active-batch outcome ownership.
use crate::{
    Completion, DeliveryError, DeliveryReceipt, EnqueueError, ExecutionError, FlushError,
    FlushFailure, FlushReport, ShutdownError, ShutdownReport, WriteError,
};
use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::{oneshot, Notify, Semaphore},
    task::AbortHandle,
};
use tracing::warn;

use crate::internal::execution::ExecutionBinding;
use crate::internal::write_outcome::{WriteOutcome, WriteResponder};
type WriteSender = WriteResponder;
type FlushSender = oneshot::Sender<Result<FlushReport, FlushError>>;
#[derive(Clone)]
enum BatchError {
    Delivery(Arc<DeliveryError>),
    Execution(ExecutionError),
}
struct Accepted<C> {
    command: C,
    sender: WriteSender,
}
struct Batch<C> {
    commands: Vec<C>,
    completion: Arc<BatchCompletion>,
}
struct BatchCompletion {
    count: usize,
    senders: Mutex<Option<Vec<WriteSender>>>,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl BatchCompletion {
    fn finish(&self, result: &Result<(), BatchError>) {
        if let Some(writes) = lock(&self.senders).take() {
            let write_result = match result {
                Ok(()) => Ok(DeliveryReceipt),
                Err(BatchError::Delivery(error)) => Err(WriteError::Delivery(error.clone())),
                Err(BatchError::Execution(error)) => Err(WriteError::Execution(*error)),
            };
            for sender in writes {
                let _ = sender.send(write_result.clone());
            }
        }
    }
}
/// One bounded summary per active flush, rather than cloning every covered
/// batch into each observer. Its cutoff is the last batch present at admission.
struct Barrier {
    through: Arc<BatchCompletion>,
    sender: Option<FlushSender>,
    delivered: u64,
    failure: Option<FlushFailure>,
    shutdown: bool,
}
impl Barrier {
    fn record(&mut self, count: u64, result: &Result<(), BatchError>) {
        match result {
            Ok(()) => self.delivered += count,
            Err(BatchError::Delivery(error)) => match &mut self.failure {
                Some(failure) => failure.failed += count,
                None => {
                    self.failure = Some(FlushFailure {
                        failed: count,
                        cause: error.clone(),
                    })
                }
            },
            Err(BatchError::Execution(error)) => {
                if let Some(sender) = self.sender.take() {
                    let _ = sender.send(Err((*error).into()));
                }
            }
        }
    }
    fn finish(&mut self) {
        if let Some(sender) = self.sender.take() {
            let result = match self.failure.take() {
                Some(failure) => Err(FlushError::Delivery(failure)),
                None => Ok(FlushReport {
                    delivered: self.delivered,
                }),
            };
            let _ = sender.send(result);
        }
    }
}
struct State<C> {
    pending: Vec<Accepted<C>>,
    ready: VecDeque<Batch<C>>,
    active: Option<Arc<BatchCompletion>>,
    barriers: Vec<Barrier>,
    outstanding: usize,
    accepting: bool,
    terminal: Option<ExecutionError>,
    depth_warned: bool,
}
impl<C> State<C> {
    fn seal(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let (commands, writes): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|item| (item.command, item.sender))
            .unzip();
        let count = commands.len();
        self.ready.push_back(Batch {
            commands,
            completion: Arc::new(BatchCompletion {
                count,
                senders: Mutex::new(Some(writes)),
            }),
        });
    }
    fn barrier(&mut self, sender: FlushSender, shutdown: bool) {
        self.seal();
        if let Some(through) = self
            .ready
            .back()
            .map(|batch| batch.completion.clone())
            .or_else(|| self.active.clone())
        {
            self.barriers.push(Barrier {
                through,
                sender: Some(sender),
                delivered: 0,
                failure: None,
                shutdown,
            });
        } else {
            let _ = sender.send(Ok(FlushReport::default()));
        }
    }
    fn finish(&mut self, batch: &Arc<BatchCompletion>, result: Result<(), BatchError>) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, batch))
        {
            self.outstanding -= batch.count;
            self.active = None;
            if self.outstanding == 0 {
                self.depth_warned = false;
            }
            batch.finish(&result);
            // Every registered barrier covers this batch: completed cutoffs
            // are removed here, before admitting any subsequent flush.
            self.barriers.retain_mut(|barrier| {
                barrier.record(batch.count as u64, &result);
                if Arc::ptr_eq(&barrier.through, batch) {
                    barrier.finish();
                    false
                } else {
                    true
                }
            });
        }
    }
}
struct Inner<C> {
    state: Mutex<State<C>>,
    wake: Notify,
    capacity: usize,
    control_limit: usize,
    shutdown_controls: Arc<Semaphore>,
    abort: Mutex<Option<AbortHandle>>,
}
impl<C> Inner<C> {
    fn stop(&self, error: ExecutionError) {
        let mut state = lock(&self.state);
        state.accepting = false;
        if state.terminal.is_some() {
            return;
        }
        state.terminal = Some(error);
        for accepted in state.pending.drain(..) {
            let _ = accepted.sender.send(Err(error.into()));
        }
        let result = Err(BatchError::Execution(error));
        if let Some(active) = state.active.take() {
            active.finish(&result);
        }
        for batch in state.ready.drain(..) {
            batch.completion.finish(&result);
        }
        for mut barrier in state.barriers.drain(..) {
            barrier.record(0, &result);
        }
        state.outstanding = 0;
        self.wake.notify_one();
    }
    fn close(&self) {
        lock(&self.state).accepting = false;
        self.wake.notify_one();
    }
}

/// Producers own no completed-outcome registry. A batch owns its write senders
/// until all its physical requests finish; barriers summarize only active work.
pub(crate) struct WriteService<C> {
    inner: Arc<Inner<C>>,
    owners: Arc<ProducerOwner<C>>,
    runtime: ExecutionBinding,
    exit: Shared<BoxFuture<'static, Result<(), ExecutionError>>>,
}
impl<C> Clone for WriteService<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owners: self.owners.clone(),
            runtime: self.runtime.clone(),
            exit: self.exit.clone(),
        }
    }
}
struct ProducerOwner<C> {
    inner: std::sync::Weak<Inner<C>>,
}
impl<C> Drop for ProducerOwner<C> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.close();
        }
    }
}
impl<C: Send + 'static> WriteService<C> {
    pub fn new<F>(capacity: usize, runtime: ExecutionBinding, process: F) -> Self
    where
        F: Fn(Vec<C>) -> BoxFuture<'static, anyhow::Result<()>> + Send + 'static,
    {
        let control_limit = capacity.clamp(1, 1024);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                pending: Vec::new(),
                ready: VecDeque::new(),
                active: None,
                barriers: Vec::new(),
                outstanding: 0,
                accepting: true,
                terminal: None,
                depth_warned: false,
            }),
            wake: Notify::new(),
            capacity,
            control_limit,
            shutdown_controls: Arc::new(Semaphore::new(control_limit)),
            abort: Mutex::new(None),
        });
        let (sender, receiver) = oneshot::channel();
        let exit = async move { receiver.await.unwrap_or(Err(ExecutionError::WorkerLost)) }
            .boxed()
            .shared();
        // Construct outside the future so cancellation before its first poll
        // still rejects pending work and notifies every accepted observer.
        let guard = WorkerGuard {
            inner: inner.clone(),
            exit: Some(sender),
        };
        let task = runtime.spawn(async move { run(guard, process).await });
        *lock(&inner.abort) = Some(task.abort_handle());
        let owners = Arc::new(ProducerOwner {
            inner: Arc::downgrade(&inner),
        });
        Self {
            inner,
            owners,
            runtime,
            exit,
        }
    }

    #[cfg(test)]
    pub fn enqueue(&self, command: C) -> Completion<DeliveryReceipt, WriteError> {
        self.enqueue_with_rejection(command, None, |_, _| {})
    }

    pub fn enqueue_with_rejection(
        &self,
        command: C,
        outcome: Option<WriteOutcome>,
        rejected: impl FnOnce(C, EnqueueError),
    ) -> Completion<DeliveryReceipt, WriteError> {
        let (sender, mut completion) = Completion::pair();
        let sender = WriteResponder::new(sender, outcome);
        let mut state = lock(&self.inner.state);
        let error = if !state.accepting {
            Some(EnqueueError::Closed)
        } else if state.outstanding >= self.inner.capacity {
            Some(EnqueueError::Full)
        } else {
            None
        };
        if let Some(error) = error {
            completion.record_issuance(Err(error.into()));
            let _ = sender.send(Err(error.into()));
            drop(state);
            rejected(command, error);
        } else {
            // Sender registration and publication have one linearization point.
            state.pending.push(Accepted { command, sender });
            state.outstanding += 1;
            if !state.depth_warned
                && state.outstanding >= self.inner.capacity - self.inner.capacity / 5
            {
                state.depth_warned = true;
                warn!(
                    outstanding = state.outstanding,
                    capacity = self.inner.capacity,
                    "delivery queue has reached 80% of capacity"
                );
            }
            self.inner.wake.notify_one();
        }
        completion
    }

    pub fn flush(&self) -> Completion<FlushReport, FlushError> {
        let (sender, mut completion) = Completion::pair();
        let mut state = lock(&self.inner.state);
        if !state.accepting {
            completion.record_issuance(Err(EnqueueError::Closed.into()));
        } else if state
            .barriers
            .iter()
            .filter(|barrier| !barrier.shutdown)
            .count()
            >= self.inner.control_limit
        {
            completion.record_issuance(Err(EnqueueError::Full.into()));
        } else {
            state.barrier(sender, false);
            self.inner.wake.notify_one();
        }
        completion
    }

    pub fn shutdown(&self, timeout: Duration) -> Completion<ShutdownReport, ShutdownError> {
        let (sender, mut completion) = Completion::pair();
        let Ok(permit) = self.inner.shutdown_controls.clone().try_acquire_owned() else {
            completion.record_issuance(Err(EnqueueError::Full.into()));
            return completion;
        };
        let (barrier_sender, barrier) = Completion::pair();
        {
            let mut state = lock(&self.inner.state);
            if let Some(error) = state.terminal {
                completion.record_issuance(Err(error.into()));
                return completion;
            }
            state.accepting = false;
            state.barrier(barrier_sender, true);
        }
        let inner = self.inner.clone();
        let exit = self.exit.clone();
        self.inner.wake.notify_one();
        self.runtime.spawn(async move {
            let _permit = permit;
            let work = async {
                let report = barrier.await;
                exit.await.map_err(ShutdownError::Execution)?;
                report
                    .map(|report| ShutdownReport {
                        delivered: report.delivered,
                    })
                    .map_err(Into::into)
            };
            let result = match tokio::time::timeout(timeout, work).await {
                Ok(result) => result,
                Err(_) => {
                    inner.stop(ExecutionError::TimedOut);
                    if let Some(abort) = lock(&inner.abort).as_ref() {
                        abort.abort();
                    }
                    Err(ShutdownError::Execution(ExecutionError::TimedOut))
                }
            };
            let _ = sender.send(result);
        });
        completion
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        lock(&self.inner.state).outstanding == 0
    }
    #[cfg(test)]
    fn outstanding(&self) -> usize {
        lock(&self.inner.state).outstanding
    }
}
struct WorkerGuard<C> {
    inner: Arc<Inner<C>>,
    exit: Option<oneshot::Sender<Result<(), ExecutionError>>>,
}
impl<C> Drop for WorkerGuard<C> {
    fn drop(&mut self) {
        if let Some(exit) = self.exit.take() {
            let error = lock(&self.inner.state)
                .terminal
                .unwrap_or(ExecutionError::WorkerLost);
            self.inner.stop(error);
            let _ = exit.send(Err(error));
        }
    }
}
async fn run<C, F>(mut guard: WorkerGuard<C>, process: F)
where
    C: Send + 'static,
    F: Fn(Vec<C>) -> BoxFuture<'static, anyhow::Result<()>>,
{
    loop {
        let wake = guard.inner.wake.notified();
        let next = {
            let mut state = lock(&guard.inner.state);
            state.seal();
            match state.ready.pop_front() {
                Some(batch) => {
                    state.active = Some(batch.completion.clone());
                    Some(batch)
                }
                None if !state.accepting => break,
                None => None,
            }
        };
        if let Some(batch) = next {
            let result = process(batch.commands)
                .await
                .map_err(|source| BatchError::Delivery(Arc::new(DeliveryError { source })));
            if let Err(BatchError::Delivery(error)) = &result {
                warn!(%error, "delivery batch failed");
            }
            lock(&guard.inner.state).finish(&batch.completion, result);
        } else {
            wake.await;
        }
    }
    if let Some(exit) = guard.exit.take() {
        let _ = exit.send(Ok(()));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::IntoFuture,
        sync::atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::mpsc;

    fn gated(
        capacity: usize,
    ) -> (
        WriteService<usize>,
        mpsc::UnboundedReceiver<Vec<usize>>,
        Arc<Semaphore>,
    ) {
        let (entered, receiver) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        let process_gate = gate.clone();
        let service = WriteService::new(
            capacity,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            move |commands| {
                let gate = process_gate.clone();
                let entered = entered.clone();
                Box::pin(async move {
                    entered.send(commands).unwrap();
                    gate.acquire().await.unwrap().forget();
                    Ok(())
                })
            },
        );
        (service, receiver, gate)
    }

    #[tokio::test]
    async fn concurrent_flushes_observe_fixed_cutoffs_including_active_work() {
        let (service, mut entered, gate) = gated(8);
        let one = service.enqueue(1);
        let first = service.flush();
        assert_eq!(entered.recv().await.unwrap(), vec![1]);
        let concurrent = service.flush();
        let two = service.enqueue(2);
        let mut second = Box::pin(service.flush().into_future());
        let three = service.enqueue(3); // beyond second's cutoff
        gate.add_permits(1);
        assert_eq!(first.await.unwrap().delivered, 1);
        assert_eq!(concurrent.await.unwrap().delivered, 1);
        one.await.unwrap();
        assert_eq!(entered.recv().await.unwrap(), vec![2]);
        assert!(futures::poll!(second.as_mut()).is_pending());
        gate.add_permits(1);
        assert_eq!(second.await.unwrap().delivered, 2);
        two.await.unwrap();
        assert_eq!(entered.recv().await.unwrap(), vec![3]);
        // The second flush finished without waiting for the later arrival.
        gate.add_permits(1);
        three.await.unwrap();
        assert_eq!(service.flush().await.unwrap().delivered, 0);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn a_new_flush_does_not_inherit_a_failed_prefix_from_an_older_active_flush() {
        let (entered, mut receiver) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        let process_gate = gate.clone();
        let service = WriteService::new(
            4,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            move |commands: Vec<usize>| {
                let entered = entered.clone();
                let gate = process_gate.clone();
                Box::pin(async move {
                    entered.send(commands.clone()).unwrap();
                    gate.acquire().await.unwrap().forget();
                    if commands == [1] {
                        anyhow::bail!("first batch failed");
                    }
                    Ok(())
                })
            },
        );
        let first = service.enqueue(1);
        assert_eq!(receiver.recv().await.unwrap(), vec![1]);
        let second = service.enqueue(2);
        let covering = service.flush();
        gate.add_permits(1);
        assert!(matches!(first.await, Err(WriteError::Delivery(_))));
        assert_eq!(receiver.recv().await.unwrap(), vec![2]);
        let fresh = service.flush();
        gate.add_permits(1);
        second.await.unwrap();
        let Err(FlushError::Delivery(failure)) = covering.await else {
            panic!("expected captured failure");
        };
        assert_eq!(failure.failed, 1);
        assert_eq!(fresh.await.unwrap().delivered, 1);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn graceful_shutdown_reports_its_failed_batch_after_notifying_all_writes() {
        let service = WriteService::new(
            4,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            |_: Vec<usize>| {
                Box::pin(async {
                    anyhow::bail!("terminal failure");
                })
            },
        );
        let first = service.enqueue(1);
        let second = service.enqueue(2);
        let shutdown = service.shutdown(Duration::from_secs(1));
        let Err(ShutdownError::Delivery(failure)) = shutdown.await else {
            panic!("expected delivery failure");
        };
        assert_eq!(failure.failed, 2);
        for completion in [first, second] {
            assert!(matches!(completion.await, Err(WriteError::Delivery(_))));
        }
        assert!(service.is_empty());
    }

    #[tokio::test]
    async fn capacity_includes_in_flight_work_and_rejection_is_observable_twice() {
        let (service, mut entered, gate) = gated(1);
        let first = service.enqueue(1);
        entered.recv().await.unwrap();
        let rejected = service.enqueue(2);
        assert!(matches!(
            &*rejected,
            Err(WriteError::Admission(EnqueueError::Full))
        ));
        assert!(matches!(
            rejected.await,
            Err(WriteError::Admission(EnqueueError::Full))
        ));
        assert_eq!(service.outstanding(), 1);
        gate.add_permits(1);
        first.await.unwrap();
        assert!(service.is_empty());
        gate.add_permits(1);
        service.enqueue(3).await.unwrap();
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn dropped_observer_does_not_cancel_automatic_delivery_or_fill_the_queue() {
        let (service, mut entered, gate) = gated(2);
        drop(service.enqueue(1));
        assert_eq!(entered.recv().await.unwrap(), vec![1]);
        let flush = service.flush();
        drop(flush); // lifecycle observers also do not cancel accepted work
        gate.add_permits(1);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(service.is_empty());
    }

    #[tokio::test]
    async fn every_failed_batch_notifies_all_members_releases_errors_and_allows_fresh_work() {
        let fail = Arc::new(AtomicBool::new(true));
        let process_fail = fail.clone();
        let service = WriteService::new(
            4,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            move |_: Vec<usize>| {
                let fail = process_fail.load(Ordering::Relaxed);
                Box::pin(async move {
                    if fail {
                        anyhow::bail!("terminal failure");
                    }
                    Ok(())
                })
            },
        );
        for _ in 0..100 {
            let first = service.enqueue(1);
            let second = service.enqueue(2);
            let flush = service.flush();
            let Err(FlushError::Delivery(failure)) = flush.await else {
                panic!("expected failure");
            };
            assert_eq!(failure.failed, 2);
            let weak = Arc::downgrade(&failure.cause);
            for completion in [first, second] {
                let Err(WriteError::Delivery(error)) = completion.await else {
                    panic!("expected failure");
                };
                assert!(Arc::ptr_eq(&error, &failure.cause));
            }
            drop(failure);
            assert!(weak.upgrade().is_none(), "SDK must release terminal errors");
            assert_eq!(service.outstanding(), 0);
            assert_eq!(service.flush().await.unwrap().delivered, 0);
        }
        fail.store(false, Ordering::Relaxed);
        let fresh = service.enqueue(3);
        assert_eq!(service.flush().await.unwrap().delivered, 1);
        fresh.await.unwrap();
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_stops_admission_immediately_drains_and_is_shared_by_clones() {
        let (service, mut entered, gate) = gated(4);
        let write = service.enqueue(1);
        let shutdown = service.shutdown(Duration::from_secs(1));
        let concurrent = service.clone().shutdown(Duration::from_secs(1));
        assert!(shutdown.is_ok());
        assert!(matches!(
            service.enqueue(2).await,
            Err(WriteError::Admission(EnqueueError::Closed))
        ));
        entered.recv().await.unwrap();
        gate.add_permits(1);
        write.await.unwrap();
        assert_eq!(shutdown.await.unwrap().delivered, 1);
        assert_eq!(concurrent.await.unwrap().delivered, 1);
        assert!(service.is_empty());
    }

    #[tokio::test]
    async fn shutdown_timeout_fails_active_work_even_when_flush_slots_are_full() {
        let (service, mut entered, _) = gated(1);
        let write = service.enqueue(1);
        entered.recv().await.unwrap();
        let flush = service.flush();
        let rejected = service.flush();
        assert!(matches!(
            &*rejected,
            Err(FlushError::Admission(EnqueueError::Full))
        ));
        let shutdown = service.shutdown(Duration::from_millis(10));
        assert!(
            shutdown.is_ok(),
            "shutdown must not compete with flush slots"
        );
        assert!(matches!(
            shutdown.await,
            Err(ShutdownError::Execution(ExecutionError::TimedOut))
        ));
        assert!(matches!(
            write.await,
            Err(WriteError::Execution(ExecutionError::TimedOut))
        ));
        assert!(matches!(
            flush.await,
            Err(FlushError::Execution(ExecutionError::TimedOut))
        ));
        assert!(service.is_empty());
        assert!(matches!(
            service.enqueue(2).await,
            Err(WriteError::Admission(EnqueueError::Closed))
        ));
    }

    #[tokio::test]
    async fn shutdown_timeout_also_retires_batches_queued_behind_active_work() {
        let (service, mut entered, _) = gated(4);
        let first = service.enqueue(1);
        entered.recv().await.unwrap();
        let second = service.enqueue(2);
        let third = service.enqueue(3);
        let flush = service.flush();
        let shutdown = service.shutdown(Duration::from_millis(10));
        assert!(matches!(
            shutdown.await,
            Err(ShutdownError::Execution(ExecutionError::TimedOut))
        ));
        for completion in [first, second, third] {
            assert!(matches!(
                completion.await,
                Err(WriteError::Execution(ExecutionError::TimedOut))
            ));
        }
        assert!(matches!(
            flush.await,
            Err(FlushError::Execution(ExecutionError::TimedOut))
        ));
        assert_eq!(service.outstanding(), 0);
        assert!(lock(&service.inner.state).barriers.is_empty());
        assert!(lock(&service.inner.state).ready.is_empty());
    }

    #[tokio::test]
    async fn very_large_write_capacity_does_not_overflow_lifecycle_control_limits() {
        let (service, _, _) = gated(usize::MAX);
        assert_eq!(service.inner.control_limit, 1024);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn worker_cancellation_before_first_poll_completes_every_accepted_sender() {
        let (service, _, _) = gated(4);
        let outcome = WriteOutcome::default();
        let first = service.enqueue_with_rejection(1, Some(outcome.clone()), |_, _| {});
        let repeated_end = outcome.observe();
        let second = service.enqueue(2);
        let flush = service.flush();
        lock(&service.inner.abort).as_ref().unwrap().abort();
        for completion in [first, second, repeated_end] {
            assert!(matches!(
                completion.await,
                Err(WriteError::Execution(ExecutionError::WorkerLost))
            ));
        }
        assert!(matches!(
            flush.await,
            Err(FlushError::Execution(ExecutionError::WorkerLost))
        ));
        assert!(matches!(
            outcome.observe().await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
        assert!(service.is_empty());
        assert!(matches!(
            service.enqueue(3).await,
            Err(WriteError::Admission(EnqueueError::Closed))
        ));
    }

    #[tokio::test]
    async fn worker_panic_notifies_active_and_pending_members() {
        let (entered, mut receiver) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        let process_gate = gate.clone();
        let service = WriteService::new(
            4,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            move |_: Vec<usize>| {
                let entered = entered.clone();
                let gate = process_gate.clone();
                Box::pin(async move {
                    entered.send(()).unwrap();
                    gate.acquire().await.unwrap().forget();
                    panic!("test processor failed");
                })
            },
        );
        let active = service.enqueue(1);
        receiver.recv().await.unwrap();
        let pending = service.enqueue(2);
        let flush = service.flush();
        gate.add_permits(1);
        for completion in [active, pending] {
            assert!(matches!(
                completion.await,
                Err(WriteError::Execution(ExecutionError::WorkerLost))
            ));
        }
        assert!(matches!(
            flush.await,
            Err(FlushError::Execution(ExecutionError::WorkerLost))
        ));
        assert!(service.is_empty());
    }

    #[tokio::test]
    async fn dropping_the_last_producers_drains_work_and_stops_the_worker() {
        let (service, mut entered, gate) = gated(4);
        let write = service.enqueue(1);
        let clone = service.clone();
        let exit = service.exit.clone();
        std::thread::scope(|scope| {
            scope.spawn(move || drop(service));
            scope.spawn(move || drop(clone));
        });
        entered.recv().await.unwrap();
        gate.add_permits(1);
        write.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), exit)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fast_workers_and_racing_flushes_cannot_lose_registered_completions() {
        let seen = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let processed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let process_seen = seen.clone();
        let process_count = processed.clone();
        let service = WriteService::new(
            1024,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            move |commands: Vec<usize>| {
                lock(&process_seen).extend(commands.iter().copied());
                process_count.fetch_add(commands.len(), Ordering::Relaxed);
                Box::pin(async { Ok(()) })
            },
        );
        let (writes, flushes) = std::thread::scope(|scope| {
            let producers: Vec<_> = (0..4)
                .map(|producer| {
                    let service = &service;
                    scope.spawn(move || {
                        (0..50)
                            .map(|item| service.enqueue(producer * 50 + item))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let flushers: Vec<_> = (0..2)
                .map(|_| {
                    let service = &service;
                    scope.spawn(move || (0..50).map(|_| service.flush()).collect::<Vec<_>>())
                })
                .collect();
            (
                producers
                    .into_iter()
                    .flat_map(|producer| producer.join().unwrap())
                    .collect::<Vec<_>>(),
                flushers
                    .into_iter()
                    .flat_map(|flusher| flusher.join().unwrap())
                    .collect::<Vec<_>>(),
            )
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            for write in writes {
                write.await.unwrap();
            }
            for flush in flushes {
                flush.await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(lock(&seen).len(), 200);
        assert_eq!(processed.load(Ordering::Relaxed), 200);
        assert!(service.is_empty());
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn racing_producers_cannot_overbook_capacity() {
        let (service, _entered, gate) = gated(8);
        let completions = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..32)
                .map(|number| {
                    let service = &service;
                    scope.spawn(move || service.enqueue(number))
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            completions
                .iter()
                .filter(|completion| completion.is_ok())
                .count(),
            8
        );
        assert_eq!(service.outstanding(), 8);
        gate.add_permits(8);
        let mut delivered = 0;
        for completion in completions {
            if completion.await.is_ok() {
                delivered += 1;
            }
        }
        assert_eq!(delivered, 8);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(service.is_empty());
    }
}
