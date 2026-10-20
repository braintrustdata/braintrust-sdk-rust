//! Fixed execution binding and ownership of a continuously driven SDK runtime.
use crate::{BraintrustError, Completion, EnqueueError, ExecutionError};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{
    runtime::{Handle, Runtime},
    sync::oneshot,
    task::JoinHandle,
};

use crate::internal::response_tasks::ResponseTasks;
#[cfg(test)]
use crate::internal::response_tasks::MAX_RESPONSE_TASKS;

#[derive(Clone)]
pub(crate) struct ExecutionBinding {
    handle: Handle,
    // Handles alone do not own a runtime. Every accepted task retains this lease.
    _owner: Option<Arc<RuntimeOwner>>,
    responses: Arc<ResponseTasks>,
}
struct RuntimeOwner {
    stop: Option<oneshot::Sender<()>>,
    #[cfg(test)]
    exit: Arc<ExitProbe>,
}
#[cfg(test)]
#[derive(Default)]
struct ExitProbe {
    done: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
}
#[cfg(test)]
impl ExitProbe {
    async fn wait(&self) {
        let wake = self.wake.notified();
        if !self.done.load(std::sync::atomic::Ordering::Acquire) {
            wake.await;
        }
    }
    fn finish(&self) {
        self.done.store(true, std::sync::atomic::Ordering::Release);
        self.wake.notify_one();
    }
}
impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl ExecutionBinding {
    pub fn new(runtime: Option<Handle>) -> Result<Self, BraintrustError> {
        if let Some(handle) = runtime {
            return Ok(Self::external(handle));
        }
        let (initialized, receiver) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = oneshot::channel();
        #[cfg(test)]
        let exit = Arc::new(ExitProbe::default());
        #[cfg(test)]
        let host_exit = exit.clone();
        std::thread::Builder::new()
            .name("braintrust-runtime".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = initialized.send(Err(error));
                        return;
                    }
                };
                if initialized.send(Ok(runtime.handle().clone())).is_err() {
                    return;
                }
                drive(runtime, stopped);
                #[cfg(test)]
                host_exit.finish();
            })
            .map_err(|error| {
                BraintrustError::InvalidConfig(format!("failed to start SDK runtime: {error}"))
            })?;
        let handle = receiver
            .recv()
            .map_err(|_| BraintrustError::Execution(ExecutionError::WorkerLost))?
            .map_err(|error| {
                BraintrustError::InvalidConfig(format!("failed to initialize SDK runtime: {error}"))
            })?;
        Ok(Self {
            handle,
            _owner: Some(Arc::new(RuntimeOwner {
                stop: Some(stop),
                #[cfg(test)]
                exit,
            })),
            responses: Arc::new(ResponseTasks::default()),
        })
    }
    pub fn external(handle: Handle) -> Self {
        Self {
            handle,
            _owner: None,
            responses: Arc::new(ResponseTasks::default()),
        }
    }
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let lease = self.clone();
        self.handle.spawn(async move {
            let _lease = lease;
            future.await
        })
    }
    pub fn spawn_blocking<F, T>(&self, work: F) -> JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let lease = self.clone();
        self.handle.spawn_blocking(move || {
            let _lease = lease;
            work()
        })
    }
    pub fn close_responses(&self) {
        self.responses.close();
    }
    pub async fn responses_drained(&self) -> Result<(), ExecutionError> {
        self.responses.drained().await
    }
    pub fn cancel_responses(&self, error: ExecutionError) {
        self.responses.cancel(error);
    }
    #[cfg(test)]
    pub fn outstanding_responses(&self) -> usize {
        self.responses.outstanding()
    }
    pub fn response<T, E, F>(&self, work: F) -> Completion<T, E>
    where
        T: Send + 'static,
        E: From<ExecutionError> + From<EnqueueError> + Send + 'static,
        F: Future<Output = Result<T, E>> + Send + 'static,
    {
        let (sender, mut completion) = Completion::pair();
        let (guard, response) = match self.responses.admit(sender) {
            Ok(admitted) => admitted,
            Err(error) => {
                completion.record_issuance(Err(error.into()));
                return completion;
            }
        };
        let id = guard.id;
        let task = self.spawn(async move {
            guard.complete(&response, work.await);
        });
        self.responses.install_abort(id, task.abort_handle());
        completion
    }
}
// Runtime destruction occurs on its host thread after leaving the runtime
// context. No join or Runtime::drop runs on an SDK worker or caller's executor.
fn drive(runtime: Runtime, stopped: oneshot::Receiver<()>) {
    runtime.block_on(async {
        let _ = stopped.await;
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{mpsc, Semaphore};

    #[tokio::test]
    async fn default_binding_ignores_ambient_runtime_and_explicit_binding_honors_it() {
        let ambient = Handle::current().id();
        let owned = ExecutionBinding::new(None).unwrap();
        let selected = owned.spawn(async { Handle::current().id() }).await.unwrap();
        assert_ne!(selected, ambient);
        assert_eq!(
            owned
                .spawn_blocking(|| Handle::current().id())
                .await
                .unwrap(),
            selected
        );
        let explicit = ExecutionBinding::new(crate::current_runtime()).unwrap();
        assert_eq!(
            explicit
                .spawn(async { Handle::current().id() })
                .await
                .unwrap(),
            ambient
        );
        assert!(owned._owner.is_some());
        assert!(explicit._owner.is_none());
    }

    #[tokio::test]
    async fn accepted_response_tasks_retain_the_owner_and_bound_unobserved_work() {
        let binding = ExecutionBinding::new(None).unwrap();
        let owner = Arc::downgrade(binding._owner.as_ref().unwrap());
        let exit = binding._owner.as_ref().unwrap().exit.clone();
        let gate = Arc::new(Semaphore::new(0));
        let (finished, mut done) = mpsc::unbounded_channel();
        let mut tokens = Vec::new();
        for _ in 0..MAX_RESPONSE_TASKS {
            let gate = gate.clone();
            let finished = finished.clone();
            let token = binding.response::<_, BraintrustError, _>(async move {
                gate.acquire().await.unwrap().forget();
                finished.send(()).unwrap();
                Ok(())
            });
            assert!(token.is_ok());
            tokens.push(token);
        }
        drop(tokens.pop());
        assert!(matches!(
            binding
                .response::<(), BraintrustError, _>(async { Ok(()) })
                .await,
            Err(BraintrustError::Admission(EnqueueError::Full))
        ));
        drop(binding);
        assert!(
            owner.upgrade().is_some(),
            "accepted tasks must retain the runtime owner"
        );
        gate.add_permits(MAX_RESPONSE_TASKS);
        for token in tokens {
            token.await.unwrap();
        }
        for _ in 0..MAX_RESPONSE_TASKS {
            done.recv().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), exit.wait())
            .await
            .unwrap();
        assert!(
            owner.upgrade().is_none(),
            "runtime host must stop after accepted work and owners retire"
        );
    }

    #[tokio::test]
    async fn close_rejects_new_responses_but_drains_unobserved_accepted_work() {
        use futures::FutureExt;
        let binding = ExecutionBinding::external(Handle::current());
        let gate = Arc::new(Semaphore::new(0));
        let work_gate = gate.clone();
        drop(binding.response::<(), BraintrustError, _>(async move {
            work_gate.acquire().await.unwrap().forget();
            Ok(())
        }));
        binding.close_responses();
        assert_eq!(binding.outstanding_responses(), 1);
        assert!(matches!(
            binding
                .response::<(), BraintrustError, _>(async { Ok(()) })
                .await,
            Err(BraintrustError::Admission(EnqueueError::Closed))
        ));
        assert!(binding.responses_drained().now_or_never().is_none());
        gate.add_permits(1);
        binding.responses_drained().await.unwrap();
        assert_eq!(binding.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn timeout_before_first_poll_notifies_tokens_and_drops_their_work() {
        struct DropSignal(Option<oneshot::Sender<()>>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                let _ = self.0.take().unwrap().send(());
            }
        }
        let binding = ExecutionBinding::external(Handle::current());
        let (sender, dropped) = oneshot::channel();
        let signal = DropSignal(Some(sender));
        let response = binding.response::<(), BraintrustError, _>(async move {
            let _signal = signal;
            std::future::pending().await
        });
        binding.close_responses();
        binding.cancel_responses(ExecutionError::TimedOut);
        assert!(matches!(
            response.await,
            Err(BraintrustError::Execution(ExecutionError::TimedOut))
        ));
        assert_eq!(binding.outstanding_responses(), 0);
        assert_eq!(
            binding.responses_drained().await,
            Err(ExecutionError::TimedOut)
        );
        tokio::time::timeout(Duration::from_secs(1), dropped)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn task_loss_retires_its_entry_and_does_not_poison_fresh_work() {
        let binding = ExecutionBinding::external(Handle::current());
        let response = binding
            .response::<(), BraintrustError, _>(async { panic!("response processor failed") });
        assert!(matches!(
            response.await,
            Err(BraintrustError::Execution(ExecutionError::WorkerLost))
        ));
        assert_eq!(binding.outstanding_responses(), 0);
        binding
            .response::<(), BraintrustError, _>(async { Ok(()) })
            .await
            .unwrap();
        binding.close_responses();
        binding.responses_drained().await.unwrap();
    }

    #[tokio::test]
    async fn racing_admission_and_close_cannot_lose_accepted_response_tasks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let binding = ExecutionBinding::external(Handle::current());
        let processed = Arc::new(AtomicUsize::new(0));
        let first = binding.response::<(), BraintrustError, _>(async { Ok(()) });
        let tokens = std::thread::scope(|threads| {
            let mut producers = Vec::new();
            for _ in 0..4 {
                let binding = binding.clone();
                let processed = processed.clone();
                producers.push(threads.spawn(move || {
                    (0..100)
                        .map(|_| {
                            let processed = processed.clone();
                            binding.response::<(), BraintrustError, _>(async move {
                                processed.fetch_add(1, Ordering::Relaxed);
                                Ok(())
                            })
                        })
                        .collect::<Vec<_>>()
                }));
            }
            let close = threads.spawn(|| binding.close_responses());
            let tokens = producers
                .into_iter()
                .flat_map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>();
            close.join().unwrap();
            tokens
        });
        let admitted = tokens.iter().filter(|token| token.is_ok()).count();
        first.await.unwrap();
        for token in tokens {
            let _ = token.await;
        }
        binding.responses_drained().await.unwrap();
        assert_eq!(processed.load(Ordering::Relaxed), admitted);
        assert_eq!(binding.outstanding_responses(), 0);
    }

    #[tokio::test]
    async fn unobserved_results_can_reenter_scheduling_from_their_destructor() {
        struct Reenter {
            binding: ExecutionBinding,
            done: Option<oneshot::Sender<()>>,
        }
        impl Drop for Reenter {
            fn drop(&mut self) {
                assert_eq!(
                    self.binding.outstanding_responses(),
                    1,
                    "drain must include result cleanup"
                );
                assert!(self
                    .binding
                    .response::<(), BraintrustError, _>(async { Ok(()) })
                    .is_ok());
                let _ = self.done.take().unwrap().send(());
            }
        }
        let binding = ExecutionBinding::external(Handle::current());
        let (done, finished) = oneshot::channel();
        let value = Reenter {
            binding: binding.clone(),
            done: Some(done),
        };
        drop(binding.response::<_, BraintrustError, _>(async move { Ok(value) }));
        tokio::time::timeout(Duration::from_secs(1), finished)
            .await
            .unwrap()
            .unwrap();
        binding.close_responses();
        binding.responses_drained().await.unwrap();
    }

    #[test]
    fn current_runtime_outside_tokio_selects_the_internal_default() {
        assert!(crate::current_runtime().is_none());
        let binding = ExecutionBinding::new(crate::current_runtime()).unwrap();
        futures::executor::block_on(async {
            binding
                .response::<_, BraintrustError, _>(async { Ok(()) })
                .await
                .unwrap();
        });
    }
}
