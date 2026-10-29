//! Producers publish into a lock-free FIFO generation. Flush swaps that generation
//! into the worker's ready list; one worker drains and processes batches in order.
//! The coordinator owns active write senders and flush cutoffs under one mutex.
use crate::{
    Completion, DeliveryError, DeliveryReceipt, EnqueueError, ExecutionError, FlushError,
    FlushFailure, FlushReport, ShutdownError, ShutdownReport, WriteError,
};
use arc_swap::ArcSwap;
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
    inner: Arc<Inner<C>>,
    owners: Arc<ProducerOwner<C>>,
    abort: AbortHandle,
    runtime: Handle,
    exit: Shared<BoxFuture<'static, Result<(), ExecutionError>>>,
}
impl<C> Clone for WriteService<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owners: self.owners.clone(),
            abort: self.abort.clone(),
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
                shutdown_pending: Vec::new(),
            }),
            ingress: ArcSwap::from_pointee(QueueEpoch::new()),
            outstanding: AtomicUsize::new(0),
            publishers: AtomicUsize::new(0),
            terminal: AtomicU8::new(0),
            depth_warned: AtomicBool::new(false),
            wake: Wake::new(),
            capacity,
            control_limit,
            shutdown_controls: Arc::new(Semaphore::new(control_limit)),
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
        let owners = Arc::new(ProducerOwner {
            inner: Arc::downgrade(&inner),
        });
        Self {
            inner,
            owners,
            abort: task.abort_handle(),
            runtime,
            exit,
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
        let (sender, mut completion) = Completion::pair();
        let reservation = self.inner.begin_publication().and_then(|()| {
            self.inner
                .reserve()
                .inspect_err(|_| self.inner.end_publication())
        });
        match reservation {
            Err(error) => {
                completion.record_issuance(Err(error.into()));
                rejected(command, error);
            }
            Ok(depth) => {
                // Publication is the linearization point: completed submissions
                // appear in FIFO order before a later flush snapshots the queue.
                self.inner.publish(Accepted { command, sender });
                if depth >= self.inner.capacity - self.inner.capacity / 5
                    && !self.inner.depth_warned.swap(true, Ordering::Relaxed)
                {
                    warn!(
                        outstanding = depth,
                        capacity = self.inner.capacity,
                        "delivery queue has reached 80% of capacity"
                    );
                }
            }
        }
        completion
    }

    pub fn flush(&self) -> Completion<FlushReport, FlushError> {
        let (sender, mut completion) = Completion::pair();
        let mut state = lock(&self.inner.state);
        if let Some(error) = self.inner.terminal_error() {
            completion.record_issuance(Err(error.into()));
        } else if self.inner.publishers.load(Ordering::SeqCst) & CLOSED != 0 {
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
            let through = self.inner.detach(&mut state);
            state.barrier(sender, false, through);
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
            if let Some(error) = self.inner.terminal_error() {
                completion.record_issuance(Err(error.into()));
                return completion;
            }
            self.inner.publishers.fetch_or(CLOSED, Ordering::SeqCst);
            state.shutdown_pending.push(barrier_sender);
            self.inner.register_shutdowns(&mut state);
        }
        let inner = self.inner.clone();
        let abort = self.abort.clone();
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
        self.inner.outstanding.load(Ordering::Acquire) == 0
    }
    #[cfg(test)]
    fn outstanding(&self) -> usize {
        self.inner.outstanding.load(Ordering::Acquire)
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
struct QueueEpoch<C> {
    queue: sdd::Queue<IngressItem<C>>,
    publishers: AtomicUsize,
}
impl<C> QueueEpoch<C> {
    fn new() -> Self {
        Self {
            queue: sdd::Queue::new(),
            publishers: AtomicUsize::new(0),
        }
    }
    fn enter(&self) -> bool {
        enter(&self.publishers)
    }
    fn close(&self) {
        self.publishers.fetch_or(CLOSED, Ordering::SeqCst);
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
    shutdown: bool,
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
    shutdown_pending: Vec<FlushSender>,
}
impl<C> State<C> {
    fn barrier(&mut self, sender: FlushSender, shutdown: bool, through: Arc<QueueEpoch<C>>) {
        self.barriers.push_back(Barrier {
            through,
            sender,
            delivered: 0,
            failure: None,
            shutdown,
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
    publishers: AtomicUsize,
    terminal: AtomicU8,
    depth_warned: AtomicBool,
    wake: Wake,
    capacity: usize,
    control_limit: usize,
    shutdown_controls: Arc<Semaphore>,
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

    fn begin_publication(&self) -> Result<(), EnqueueError> {
        enter(&self.publishers)
            .then_some(())
            .ok_or(EnqueueError::Closed)
    }

    fn end_publication(&self) {
        self.publishers.fetch_sub(1, Ordering::SeqCst);
        self.wake.notify_one();
    }

    #[allow(deprecated, reason = "try_update requires Rust 1.95")]
    fn reserve(&self) -> Result<usize, EnqueueError> {
        self.outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |depth| {
                (depth < self.capacity).then(|| depth + 1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| EnqueueError::Full)
    }

    fn release(&self, count: usize) {
        if count != 0 && self.outstanding.fetch_sub(count, Ordering::AcqRel) == count {
            self.depth_warned.store(false, Ordering::Relaxed);
        }
    }

    fn detach(&self, state: &mut State<C>) -> Arc<QueueEpoch<C>> {
        let epoch = self.ingress.swap(Arc::new(QueueEpoch::new()));
        epoch.close();
        state.ready.push_back(epoch.clone());
        epoch
    }

    fn register_shutdowns(&self, state: &mut State<C>) -> bool {
        if state.shutdown_pending.is_empty() {
            return true;
        }
        if self.publishers.load(Ordering::SeqCst) & !CLOSED != 0 {
            return false;
        }
        let through = self.detach(state);
        for sender in std::mem::take(&mut state.shutdown_pending) {
            state.barrier(sender, true, through.clone());
        }
        true
    }

    fn fail_epoch(&self, epoch: &QueueEpoch<C>, error: ExecutionError) {
        while let Some(entry) = epoch.queue.pop() {
            let accepted = entry.take().expect("each queue entry is taken once");
            let _ = accepted.sender.send(Err(error.into()));
            self.release(1);
        }
    }

    fn publish(&self, accepted: Accepted<C>)
    where
        C: 'static,
    {
        loop {
            // Keep the generation borrowed only for publication; unlike an
            // owned load this avoids contending on its Arc reference count on
            // every submission. A concurrent swap keeps the guard alive.
            let epoch = self.ingress.load();
            if epoch.enter() {
                self.publish_to_epoch(&epoch, accepted);
                return;
            }
            if let Some(error) = self.terminal_error() {
                let _ = accepted.sender.send(Err(error.into()));
                self.release(1);
                self.end_publication();
                return;
            }
        }
    }

    fn publish_to_epoch(&self, epoch: &QueueEpoch<C>, accepted: Accepted<C>)
    where
        C: 'static,
    {
        epoch.queue.push(AtomicCell::new(Some(Box::new(accepted))));
        epoch.publishers.fetch_sub(1, Ordering::SeqCst);
        let terminal = self.terminal_error();
        self.end_publication();
        if let Some(error) = terminal {
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
        self.publishers.fetch_or(CLOSED, Ordering::SeqCst);
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
        for sender in state.shutdown_pending.drain(..) {
            let _ = sender.send(Err(error.into()));
        }
        drop(state);
        let current = self.ingress.load_full();
        current.close();
        self.fail_epoch(&current, error);
        self.wake.notify_one();
        error
    }
    fn close(&self) {
        self.publishers.fetch_or(CLOSED, Ordering::SeqCst);
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
            } else if !guard.inner.register_shutdowns(&mut state) {
                None
            } else {
                // Check closure before queue contents: if the last producer
                // finishes after this snapshot, its wake triggers another pass.
                let closed = guard.inner.publishers.load(Ordering::SeqCst) == CLOSED;
                if state.ready.is_empty() && !guard.inner.ingress.load().queue.is_empty() {
                    guard.inner.detach(&mut state);
                }
                if state.ready.front().is_some_and(|epoch| epoch.settled()) {
                    let epoch = state.ready.pop_front().expect("checked above");
                    state.active = Some(Batch {
                        epoch: epoch.clone(),
                        senders: Vec::new(),
                    });
                    Some(epoch)
                } else if state.ready.is_empty() && closed {
                    break;
                } else {
                    None
                }
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
        loop {
            let wake = guard.inner.wake.notified();
            let finished = {
                let mut state = lock(&guard.inner.state);
                if guard.inner.terminal_error().is_some() {
                    break;
                } else if guard.inner.register_shutdowns(&mut state) {
                    let count = state.finish(&result);
                    guard.inner.release(count);
                    true
                } else {
                    false
                }
            };
            if finished {
                break;
            }
            wake.await;
        }
    }
    if let Some(exit) = guard.exit.take() {
        let result = guard.inner.terminal_error().map_or(Ok(()), Err);
        let _ = exit.send(result);
    }
}
#[cfg(test)]
mod tests;
