//! Producers publish into a lock-free FIFO generation. Flush swaps that generation
//! into the worker's ready list; one worker drains and processes batches in order.
//! The coordinator owns active write senders and flush cutoffs under one mutex.
use crate::{
    Completion, DeliveryError, DeliveryReceipt, EnqueueError, ExecutionError, FlushError,
    FlushFailure, FlushReport, ShutdownError, ShutdownReport, WriteError,
};
use arc_swap::{ArcSwap, Guard};
use crossbeam_utils::atomic::AtomicCell;
use futures::{
    future::{BoxFuture, Shared},
    task::AtomicWaker,
    FutureExt,
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::{oneshot, Semaphore},
    task::AbortHandle,
};
use tracing::warn;

type WriteSender = oneshot::Sender<Result<DeliveryReceipt, WriteError>>;
type FlushSender = oneshot::Sender<Result<FlushReport, FlushError>>;
/// Generic delivery coordination for an injected batch processor. Producers own
/// no completed-outcome registry: a batch owns its write senders until all its
/// physical requests finish, and barriers summarize only active work.
pub(crate) struct WriteService<C> {
    producer: Arc<Producer<C>>,
}
impl<C> Clone for WriteService<C> {
    fn clone(&self) -> Self {
        Self {
            producer: self.producer.clone(),
        }
    }
}
/// Only producer handles retain this owner. The worker retains `Inner` alone,
/// so dropping the last producer closes admission while queued work can drain.
struct Producer<C> {
    inner: Arc<Inner<C>>,
    abort: AbortHandle,
    runtime: Handle,
    exit: Shared<BoxFuture<'static, Result<(), ExecutionError>>>,
    control_limit: usize,
    shutdown_controls: Arc<Semaphore>,
}
impl<C> Drop for Producer<C> {
    fn drop(&mut self) {
        self.inner.close(&mut lock(&self.inner.state));
    }
}
impl<C: Send + 'static> WriteService<C> {
    pub fn new<F>(capacity: usize, runtime: Handle, process: F) -> Self
    where
        F: Fn(Vec<C>) -> BoxFuture<'static, anyhow::Result<()>> + Send + 'static,
    {
        assert!(
            IngressItem::<C>::is_lock_free(),
            "delivery queue requires lock-free pointer-sized atomics"
        );
        let control_limit = capacity.clamp(1, 1024);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                ready: VecDeque::new(),
                active: None,
                barriers: VecDeque::new(),
            }),
            ingress: ArcSwap::from_pointee(QueueEpoch::new(false)),
            outstanding: AtomicUsize::new(0),
            terminal: AtomicU8::new(0),
            depth_warned: AtomicBool::new(false),
            wake: Wake::new(),
            capacity,
            #[cfg(test)]
            after_take: Mutex::new(None),
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
        Self {
            producer: Arc::new(Producer {
                inner,
                abort: task.abort_handle(),
                runtime,
                exit,
                control_limit,
                shutdown_controls: Arc::new(Semaphore::new(control_limit)),
            }),
        }
    }

    #[cfg(test)]
    pub fn enqueue(&self, command: C) -> Completion<DeliveryReceipt, WriteError> {
        self.enqueue_with_rejection(command, |_, _| {})
    }

    pub fn enqueue_with_rejection(
        &self,
        command: C,
        rejected: impl FnOnce(C, EnqueueError),
    ) -> Completion<DeliveryReceipt, WriteError> {
        let producer = &self.producer;
        let (sender, mut completion) = Completion::pair();
        match producer.inner.admit() {
            Err(error) => {
                completion.record_issuance(Err(error.into()));
                rejected(command, error);
            }
            Ok((epoch, depth)) => {
                // Admission fixes the snapshot membership; publication keeps
                // FIFO order within that generation.
                producer.inner.publish(&epoch, Accepted { command, sender });
                if depth >= producer.inner.capacity - producer.inner.capacity / 5
                    && !producer.inner.depth_warned.swap(true, Ordering::Relaxed)
                {
                    warn!(
                        outstanding = depth,
                        capacity = producer.inner.capacity,
                        "delivery queue has reached 80% of capacity"
                    );
                }
            }
        }
        completion
    }

    pub fn flush(&self) -> Completion<FlushReport, FlushError> {
        let producer = &self.producer;
        let (sender, mut completion) = Completion::pair();
        let mut state = lock(&producer.inner.state);
        if let Some(error) = producer.inner.terminal_error() {
            completion.record_issuance(Err(error.into()));
        } else if producer.inner.ingress.load().closed() {
            completion.record_issuance(Err(EnqueueError::Closed.into()));
        } else if state.barriers.len() >= producer.control_limit {
            completion.record_issuance(Err(EnqueueError::Full.into()));
        } else {
            let through = producer.inner.detach(&mut state, false);
            state.barrier(sender, through);
            producer.inner.wake.notify_one();
        }
        completion
    }

    pub fn shutdown(&self, timeout: Duration) -> Completion<ShutdownReport, ShutdownError> {
        let producer = &self.producer;
        let (sender, mut completion) = Completion::pair();
        let Ok(permit) = producer.shutdown_controls.clone().try_acquire_owned() else {
            completion.record_issuance(Err(EnqueueError::Full.into()));
            return completion;
        };
        let (barrier_sender, barrier) = Completion::pair();
        {
            let mut state = lock(&producer.inner.state);
            if let Some(error) = producer.inner.terminal_error() {
                completion.record_issuance(Err(error.into()));
                return completion;
            }
            producer.inner.close(&mut state);
            let through = state
                .ready
                .back()
                .cloned()
                .or_else(|| state.active.as_ref().map(|batch| batch.epoch.clone()));
            if let Some(through) = through {
                state.barrier(barrier_sender, through);
            } else {
                let _ = barrier_sender.send(Ok(FlushReport::default()));
            }
        }
        let inner = producer.inner.clone();
        let abort = producer.abort.clone();
        let exit = producer.exit.clone();
        producer.runtime.spawn(async move {
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
                    abort.abort();
                    Err(ShutdownError::Execution(ExecutionError::TimedOut))
                }
            };
            let _ = sender.send(result);
        });
        completion
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.producer.inner.outstanding.load(Ordering::Acquire) == 0
    }
    #[cfg(test)]
    fn outstanding(&self) -> usize {
        self.producer.inner.outstanding.load(Ordering::Acquire)
    }
}

const CLOSED: usize = 1 << (usize::BITS - 1);

#[allow(deprecated, reason = "try_update requires Rust 1.95")]
fn enter(publishers: &AtomicUsize) -> bool {
    publishers
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count & CLOSED == 0).then(|| count + 1)
        })
        .is_ok()
}

/// One worker consumes coalesced notifications; producers never lock a wait list.
struct Wake {
    ready: AtomicBool,
    waker: AtomicWaker,
}
impl Wake {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }
    fn notify_one(&self) {
        self.ready.store(true, Ordering::Release);
        self.waker.wake();
    }
    async fn notified(&self) {
        futures::future::poll_fn(|cx| {
            if self.ready.swap(false, Ordering::AcqRel) {
                return std::task::Poll::Ready(());
            }
            self.waker.register(cx.waker());
            if self.ready.swap(false, Ordering::AcqRel) {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await
    }
}
struct Accepted<C> {
    command: C,
    sender: WriteSender,
}
type IngressItem<C> = AtomicCell<Option<Box<Accepted<C>>>>;
type EpochGuard<C> = Guard<Arc<QueueEpoch<C>>>;
struct QueueEpoch<C> {
    queue: sdd::Queue<IngressItem<C>>,
    publishers: AtomicUsize,
}
impl<C> QueueEpoch<C> {
    fn new(closed: bool) -> Self {
        Self {
            queue: sdd::Queue::new(),
            publishers: AtomicUsize::new(if closed { CLOSED } else { 0 }),
        }
    }
    fn enter(&self) -> bool {
        enter(&self.publishers)
    }
    fn close(&self) {
        self.publishers.fetch_or(CLOSED, Ordering::SeqCst);
    }
    fn closed(&self) -> bool {
        self.publishers.load(Ordering::SeqCst) & CLOSED != 0
    }
    fn settled(&self) -> bool {
        self.publishers.load(Ordering::SeqCst) & !CLOSED == 0
    }
}
struct Batch<C> {
    epoch: Arc<QueueEpoch<C>>,
    senders: Vec<WriteSender>,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn finish_writes(writes: Vec<WriteSender>, result: Result<(), WriteError>) {
    let write_result = result.map(|()| DeliveryReceipt::new());
    for sender in writes {
        let _ = sender.send(write_result.clone());
    }
}
/// One bounded summary per active flush, rather than cloning every covered
/// batch into each observer. Its cutoff is the last batch present at admission.
struct Barrier<C> {
    through: Arc<QueueEpoch<C>>,
    sender: FlushSender,
    delivered: u64,
    failure: Option<FlushFailure>,
}
impl<C> Barrier<C> {
    fn record(&mut self, count: u64, result: &Result<(), Arc<DeliveryError>>) {
        match result {
            Ok(()) => self.delivered += count,
            Err(error) => match &mut self.failure {
                Some(failure) => failure.failed += count,
                None => {
                    self.failure = Some(FlushFailure {
                        failed: count,
                        cause: error.clone(),
                    })
                }
            },
        }
    }
    fn finish(self) {
        let result = match self.failure {
            Some(failure) => Err(FlushError::Delivery(failure)),
            None => Ok(FlushReport {
                delivered: self.delivered,
            }),
        };
        let _ = self.sender.send(result);
    }
}
struct State<C> {
    ready: VecDeque<Arc<QueueEpoch<C>>>,
    active: Option<Batch<C>>,
    barriers: VecDeque<Barrier<C>>,
}
impl<C> State<C> {
    fn barrier(&mut self, sender: FlushSender, through: Arc<QueueEpoch<C>>) {
        self.barriers.push_back(Barrier {
            through,
            sender,
            delivered: 0,
            failure: None,
        });
    }
    fn finish(&mut self, result: &Result<(), Arc<DeliveryError>>) -> usize {
        let active = self.active.take().expect("active batch");
        let count = active.senders.len();
        finish_writes(active.senders, result.clone().map_err(WriteError::Delivery));
        // Every registered barrier covers this batch: completed cutoffs
        // are removed here, before admitting any subsequent flush.
        for barrier in &mut self.barriers {
            barrier.record(count as u64, result);
        }
        // Cutoffs follow admission order, so completed observers leave from
        // the front and their senders never need an intermediate empty state.
        while self
            .barriers
            .front()
            .is_some_and(|barrier| Arc::ptr_eq(&barrier.through, &active.epoch))
        {
            self.barriers.pop_front().expect("checked above").finish();
        }
        count
    }
}
struct Inner<C> {
    state: Mutex<State<C>>,
    ingress: ArcSwap<QueueEpoch<C>>,
    outstanding: AtomicUsize,
    terminal: AtomicU8,
    depth_warned: AtomicBool,
    wake: Wake,
    capacity: usize,
    #[cfg(test)]
    after_take: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}
impl<C> Inner<C> {
    fn terminal_error(&self) -> Option<ExecutionError> {
        match self.terminal.load(Ordering::SeqCst) {
            0 => None,
            2 => Some(ExecutionError::TimedOut),
            _ => Some(ExecutionError::WorkerLost),
        }
    }

    #[allow(deprecated, reason = "try_update requires Rust 1.95")]
    fn admit(&self) -> Result<(EpochGuard<C>, usize), EnqueueError> {
        loop {
            // Enrollment binds this write to one flush/shutdown cutoff before
            // admission succeeds. A closed current generation rejects new work;
            // a stale generation means a concurrent flush advanced the queue.
            let epoch = self.ingress.load();
            if !epoch.enter() {
                if Arc::ptr_eq(&epoch, &self.ingress.load()) {
                    return Err(EnqueueError::Closed);
                }
                continue;
            }
            match self
                .outstanding
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |depth| {
                    (depth < self.capacity).then(|| depth + 1)
                }) {
                Ok(previous) => return Ok((epoch, previous + 1)),
                Err(_) => {
                    self.end_publication(&epoch);
                    return Err(EnqueueError::Full);
                }
            }
        }
    }

    fn end_publication(&self, epoch: &QueueEpoch<C>) {
        epoch.publishers.fetch_sub(1, Ordering::SeqCst);
        self.wake.notify_one();
    }

    fn release(&self, count: usize) {
        if count != 0 && self.outstanding.fetch_sub(count, Ordering::AcqRel) == count {
            self.depth_warned.store(false, Ordering::Relaxed);
        }
    }

    fn detach(&self, state: &mut State<C>, closing: bool) -> Arc<QueueEpoch<C>> {
        let epoch = self.ingress.swap(Arc::new(QueueEpoch::new(closing)));
        epoch.close();
        state.ready.push_back(epoch.clone());
        epoch
    }

    fn fail_epoch(&self, epoch: &QueueEpoch<C>, error: ExecutionError) {
        while let Some(entry) = epoch.queue.pop() {
            let accepted = entry.take().expect("each queue entry is taken once");
            let _ = accepted.sender.send(Err(error.into()));
            self.release(1);
        }
    }

    fn publish(&self, epoch: &QueueEpoch<C>, accepted: Accepted<C>)
    where
        C: 'static,
    {
        epoch.queue.push(AtomicCell::new(Some(Box::new(accepted))));
        // Retire publication before checking terminal state: either stop()
        // observes the queued item or this publisher performs its cleanup.
        self.end_publication(epoch);
        if let Some(error) = self.terminal_error() {
            self.fail_epoch(epoch, error);
        }
    }

    fn stop(&self, error: ExecutionError) -> ExecutionError {
        let code = match error {
            ExecutionError::WorkerLost => 1,
            ExecutionError::TimedOut => 2,
        };
        let mut state = lock(&self.state);
        if let Some(error) = self.terminal_error() {
            return error;
        }
        // Serialize the first terminal transition with batch completion so a
        // returning processor cannot report success after shutdown has failed it.
        self.terminal.store(code, Ordering::SeqCst);
        self.close(&mut state);
        if let Some(active) = state.active.take() {
            let count = active.senders.len();
            finish_writes(active.senders, Err(error.into()));
            self.release(count);
            self.fail_epoch(&active.epoch, error);
        }
        for epoch in state.ready.drain(..) {
            self.fail_epoch(&epoch, error);
        }
        for barrier in state.barriers.drain(..) {
            let _ = barrier.sender.send(Err(error.into()));
        }
        error
    }
    fn close(&self, state: &mut State<C>) {
        if !self.ingress.load().closed() {
            self.detach(state, true);
        }
        self.wake.notify_one();
    }
}

struct WorkerGuard<C> {
    inner: Arc<Inner<C>>,
    exit: Option<oneshot::Sender<Result<(), ExecutionError>>>,
}
impl<C> Drop for WorkerGuard<C> {
    fn drop(&mut self) {
        if let Some(exit) = self.exit.take() {
            let error = self.inner.stop(ExecutionError::WorkerLost);
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
            if guard.inner.terminal_error().is_some() {
                break;
            }
            if state.ready.is_empty() && !guard.inner.ingress.load().queue.is_empty() {
                guard.inner.detach(&mut state, false);
            }
            if state.ready.front().is_some_and(|epoch| epoch.settled()) {
                let epoch = state.ready.pop_front().expect("checked above");
                state.active = Some(Batch {
                    epoch: epoch.clone(),
                    senders: Vec::new(),
                });
                Some(epoch)
            } else if state.ready.is_empty() && guard.inner.ingress.load().closed() {
                break;
            } else {
                None
            }
        };
        let Some(epoch) = next else {
            wake.await;
            continue;
        };
        // The generation has been detached. Drain it without holding the
        // coordinator lock, so concurrent flushes can establish cutoffs.
        let mut commands = Vec::new();
        let mut senders = Vec::new();
        while let Some(entry) = epoch.queue.pop() {
            let accepted = entry.take().expect("each queue entry is taken once");
            commands.push(accepted.command);
            senders.push(accepted.sender);
            #[cfg(test)]
            if let Some((entered, resume)) = lock(&guard.inner.after_take).take() {
                entered.send(()).unwrap();
                resume.recv().unwrap();
            }
        }
        {
            let mut state = lock(&guard.inner.state);
            if let Some(error) = guard.inner.terminal_error() {
                drop(state);
                // stop() drains the rest of the epoch; these entries were
                // already taken and remain the worker's responsibility.
                finish_writes(senders, Err(error.into()));
                guard.inner.release(commands.len());
                break;
            }
            state.active.as_mut().expect("active batch").senders = senders;
        }
        let result = if commands.is_empty() {
            Ok(())
        } else {
            process(commands)
                .await
                .map_err(|source| Arc::new(DeliveryError { source }))
        };
        if let Err(error) = &result {
            warn!(%error, "delivery batch failed");
        }
        let mut state = lock(&guard.inner.state);
        if guard.inner.terminal_error().is_some() {
            break;
        }
        let count = state.finish(&result);
        guard.inner.release(count);
    }
    if let Some(exit) = guard.exit.take() {
        let result = guard.inner.terminal_error().map_or(Ok(()), Err);
        let _ = exit.send(result);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admission_does_not_wait_for_coordinator_lock() {
        let (service, mut entered, gate) = gated(4);
        let coordinator = lock(&service.producer.inner.state);
        let producer = service.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let completion = producer.enqueue(7);
            sent.send(completion.submission().is_ok()).unwrap();
        });
        let admitted = received.recv_timeout(Duration::from_secs(2));
        drop(coordinator);
        thread.join().unwrap();
        assert!(admitted.unwrap());
        assert_eq!(entered.recv().await.unwrap(), vec![7]);
        gate.add_permits(1);
        service.shutdown(Duration::from_secs(2)).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_includes_a_publisher_already_in_progress() {
        let (service, mut entered, gate) = gated(4);
        let first = service.enqueue(1);
        assert_eq!(entered.recv().await.unwrap(), vec![1]);

        let (epoch, _) = service.producer.inner.admit().unwrap();
        let (sender, second) = Completion::pair();
        let shutdown = service.shutdown(Duration::from_secs(2));
        let mut shutdown = Box::pin(shutdown.into_future());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
                .await
                .is_err()
        );
        service
            .producer
            .inner
            .publish(&epoch, Accepted { command: 2, sender });
        gate.add_permits(1);
        assert!(first.await.is_ok());
        assert_eq!(entered.recv().await.unwrap(), vec![2]);
        gate.add_permits(1);
        assert!(second.await.is_ok());
        assert_eq!(shutdown.await.unwrap().delivered(), 2);
    }

    #[tokio::test]
    async fn flush_swaps_generation_and_waits_for_its_late_publisher() {
        let (service, mut entered, gate) = gated(8);
        let first = service.enqueue(1);
        assert_eq!(entered.recv().await.unwrap(), vec![1]);

        let (old_epoch, _) = service.producer.inner.admit().unwrap();
        let (sender, before) = Completion::pair();
        let mut flush = Box::pin(service.flush().into_future());
        assert!(!Arc::ptr_eq(
            &old_epoch,
            &service.producer.inner.ingress.load_full()
        ));
        let after = service.enqueue(3);
        service
            .producer
            .inner
            .publish(&old_epoch, Accepted { command: 2, sender });

        gate.add_permits(1);
        first.await.unwrap();
        assert_eq!(entered.recv().await.unwrap(), vec![2]);
        assert!(futures::poll!(flush.as_mut()).is_pending());
        gate.add_permits(1);
        before.await.unwrap();
        assert_eq!(flush.await.unwrap().delivered(), 2);
        assert_eq!(entered.recv().await.unwrap(), vec![3]);
        gate.add_permits(1);
        after.await.unwrap();
        service.shutdown(Duration::from_secs(2)).await.unwrap();
    }

    #[tokio::test]
    async fn flush_detaches_without_draining_the_queue() {
        let (service, mut entered, gate) = gated(8);
        let writes: Vec<_> = (0..4).map(|item| service.enqueue(item)).collect();
        let old_epoch = service.producer.inner.ingress.load_full();
        let flush = service.flush();
        assert!(!Arc::ptr_eq(
            &old_epoch,
            &service.producer.inner.ingress.load_full()
        ));
        // On the current-thread runtime, the worker has not run yet. Flush
        // must not have taken the first item out of the detached queue.
        assert!(old_epoch.queue.peek(&sdd::Guard::new()).is_some());
        assert_eq!(entered.recv().await.unwrap(), vec![0, 1, 2, 3]);
        gate.add_permits(1);
        assert_eq!(flush.await.unwrap().delivered(), 4);
        for write in writes {
            write.await.unwrap();
        }
        service.shutdown(Duration::from_secs(2)).await.unwrap();
    }

    #[tokio::test]
    async fn worker_failure_completes_a_late_publisher() {
        let (service, mut entered, _gate) = gated(4);
        let first = service.enqueue(1);
        assert_eq!(entered.recv().await.unwrap(), vec![1]);
        let (old_epoch, _) = service.producer.inner.admit().unwrap();
        let (sender, late) = Completion::pair();
        service.producer.abort.abort();
        assert!(matches!(
            first.await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
        service
            .producer
            .inner
            .publish(&old_epoch, Accepted { command: 2, sender });
        assert!(matches!(
            late.await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
        assert!(service.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_fails_items_taken_and_not_yet_taken_by_worker() {
        let (service, _entered, _gate) = gated(4);
        let (taken, wait_taken) = std::sync::mpsc::channel();
        let (resume, wait_resume) = std::sync::mpsc::channel();
        *lock(&service.producer.inner.after_take) = Some((taken, wait_resume));
        let coordinator = lock(&service.producer.inner.state);
        let first = service.enqueue(1);
        let second = service.enqueue(2);
        drop(coordinator);
        wait_taken.recv_timeout(Duration::from_secs(2)).unwrap();
        let shutdown = service.shutdown(Duration::from_millis(50));
        assert!(matches!(
            shutdown.await,
            Err(ShutdownError::Execution(ExecutionError::TimedOut))
        ));
        resume.send(()).unwrap();
        for completion in [first, second] {
            assert!(matches!(
                completion.await,
                Err(WriteError::Execution(ExecutionError::TimedOut))
            ));
        }
        let _ = service.producer.exit.clone().await;
        assert!(service.is_empty());
    }

    #[tokio::test]
    async fn terminal_stop_is_preserved_when_the_processor_returns_success() {
        let (service, mut entered, gate) = gated(4);
        let active = service.enqueue(1);
        assert_eq!(entered.recv().await.unwrap(), vec![1]);
        let pending = service.enqueue(2);
        let flush = service.flush();

        service.producer.inner.stop(ExecutionError::TimedOut);
        gate.add_permits(1); // The processor returns Ok after its batch was failed.
        for write in [active, pending] {
            assert!(matches!(
                write.await,
                Err(WriteError::Execution(ExecutionError::TimedOut))
            ));
        }
        assert!(matches!(
            flush.await,
            Err(FlushError::Execution(ExecutionError::TimedOut))
        ));
        assert_eq!(
            service.producer.exit.clone().await,
            Err(ExecutionError::TimedOut)
        );
        assert!(service.is_empty());
        assert!(matches!(
            service.flush().submission(),
            Err(FlushError::Execution(ExecutionError::TimedOut))
        ));
        assert!(matches!(
            service.shutdown(Duration::from_secs(1)).await,
            Err(ShutdownError::Execution(ExecutionError::TimedOut))
        ));
    }

    #[tokio::test]
    async fn repeated_shutdown_after_worker_exit_succeeds_without_new_work() {
        let (service, _entered, _gate) = gated(4);
        assert_eq!(
            service
                .shutdown(Duration::from_secs(1))
                .await
                .unwrap()
                .delivered(),
            0
        );
        assert_eq!(
            service
                .shutdown(Duration::from_millis(50))
                .await
                .unwrap()
                .delivered(),
            0
        );
        assert!(service.is_empty());
    }

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
        let service = WriteService::new(capacity, Handle::current(), move |commands| {
            let gate = process_gate.clone();
            let entered = entered.clone();
            Box::pin(async move {
                entered.send(commands).unwrap();
                gate.acquire().await.unwrap().forget();
                Ok(())
            })
        });
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
        let service = WriteService::new(4, Handle::current(), move |commands: Vec<usize>| {
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
        });
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
        let service = WriteService::new(4, Handle::current(), |_: Vec<usize>| {
            Box::pin(async {
                anyhow::bail!("terminal failure");
            })
        });
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
        let service = WriteService::new(4, Handle::current(), move |_: Vec<usize>| {
            let fail = process_fail.load(Ordering::Relaxed);
            Box::pin(async move {
                if fail {
                    anyhow::bail!("terminal failure");
                }
                Ok(())
            })
        });
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
        assert!(lock(&service.producer.inner.state).barriers.is_empty());
        assert!(lock(&service.producer.inner.state).ready.is_empty());
    }

    #[tokio::test]
    async fn very_large_write_capacity_does_not_overflow_lifecycle_control_limits() {
        let (service, _, _) = gated(usize::MAX);
        assert_eq!(service.producer.control_limit, 1024);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn worker_cancellation_before_first_poll_completes_every_accepted_sender() {
        let (service, _, _) = gated(4);
        let first = service.enqueue(1);
        let second = service.enqueue(2);
        let flush = service.flush();
        service.producer.abort.abort();
        for completion in [first, second] {
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
        let service = WriteService::new(4, Handle::current(), move |_: Vec<usize>| {
            let entered = entered.clone();
            let gate = process_gate.clone();
            Box::pin(async move {
                entered.send(()).unwrap();
                gate.acquire().await.unwrap().forget();
                panic!("test processor failed");
            })
        });
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_last_producers_drains_work_and_stops_the_worker() {
        for _ in 0..64 {
            let (service, mut entered, gate) = gated(4);
            let write = service.enqueue(1);
            let clone = service.clone();
            let exit = service.producer.exit.clone();
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
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fast_workers_and_racing_flushes_cannot_lose_registered_completions() {
        let seen = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let processed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let process_seen = seen.clone();
        let process_count = processed.clone();
        let service = WriteService::new(1024, Handle::current(), move |commands: Vec<usize>| {
            lock(&process_seen).extend(commands.iter().copied());
            process_count.fetch_add(commands.len(), Ordering::Relaxed);
            Box::pin(async { Ok(()) })
        });
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
