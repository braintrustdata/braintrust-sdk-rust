//! One client shutdown coordinates response tasks and ingestion under one deadline.
use crate::internal::execution::ExecutionBinding;
use crate::internal::shared_outcome::{SharedOutcome, SharedResponder};
use crate::log_queue::LogQueue;
use crate::{Completion, ExecutionError, ShutdownError, ShutdownReport};
use std::sync::Mutex;
use std::time::Duration;

type Outcome = SharedOutcome<ShutdownReport, ShutdownError>;
pub(crate) struct Lifecycle {
    execution: ExecutionBinding,
    writes: LogQueue,
    shutdown: Mutex<Option<Outcome>>,
}
impl Lifecycle {
    pub fn new(execution: ExecutionBinding, writes: LogQueue) -> Self {
        Self {
            execution,
            writes,
            shutdown: Mutex::new(None),
        }
    }
    pub fn shutdown(&self, timeout: Duration) -> Completion<ShutdownReport, ShutdownError> {
        let mut state = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(outcome) = &*state {
            return outcome.observe();
        }
        let outcome = Outcome::default();
        let (sender, completion) = Completion::pair();
        let responder = SharedResponder::new(sender, Some(outcome.clone()));
        *state = Some(outcome);
        // Close both admission domains before returning any shutdown observer.
        self.execution.close_responses();
        let barrier = self.writes.begin_shutdown();
        let execution = self.execution.clone();
        let queue = self.writes.clone();
        let deadline = tokio::time::Instant::now().checked_add(timeout);
        self.execution.spawn(async move {
            let work = async {
                let responses = execution.responses_drained().await;
                let writes = queue.finish_shutdown(barrier).await;
                responses.map_err(ShutdownError::Execution)?;
                writes
            };
            let result = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, work).await,
                None => Ok(work.await),
            };
            let result = match result {
                Ok(result) => result,
                Err(_) => {
                    execution.cancel_responses(ExecutionError::TimedOut);
                    queue.cancel(ExecutionError::TimedOut);
                    Err(ShutdownError::Execution(ExecutionError::TimedOut))
                }
            };
            let _ = responder.send(result);
        });
        completion
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ports::SpanSubmitter;
    use crate::log_queue::LogQueueConfig;
    use crate::{BraintrustError, WriteError};
    use std::sync::Arc;
    use tokio::sync::{oneshot, Semaphore};

    #[tokio::test]
    async fn shutdown_remembers_retired_failures_while_responses_drain_without_observers() {
        let execution = ExecutionBinding::external(tokio::runtime::Handle::current());
        let queue = LogQueue::new(
            LogQueueConfig::default(),
            reqwest::Client::new(),
            execution.clone(),
        );
        let lifecycle = Lifecycle::new(execution.clone(), queue.clone());
        let operation = queue.operation().unwrap();
        let (retired, observed) = oneshot::channel();
        let gate = Arc::new(Semaphore::new(0));
        let response_gate = gate.clone();
        let response = execution.response::<(), BraintrustError, _>(async move {
            let write = operation.submitter.submit_deferred(
                "failed".into(),
                Box::pin(async {
                    Err(BraintrustError::InvalidConfig("preparation failed".into()))
                }),
            );
            assert!(matches!(write.await, Err(WriteError::Delivery(_))));
            retired.send(()).unwrap();
            response_gate.acquire().await.unwrap().forget();
            drop(operation);
            Ok(())
        });
        drop(lifecycle.shutdown(Duration::from_secs(5)));
        observed.await.unwrap();
        assert!(queue.is_empty());
        assert!(matches!(
            queue.flush().await,
            Err(crate::FlushError::Admission(crate::EnqueueError::Closed))
        ));
        gate.add_permits(1);
        response.await.unwrap();
        // A new observer must see the original failure even though the batch
        // retired before the response, and the first observer was dropped.
        let Err(ShutdownError::Delivery(failure)) = lifecycle.shutdown(Duration::ZERO).await else {
            panic!("shutdown lost its retired failure");
        };
        assert_eq!(failure.failed, 1);
    }

    #[tokio::test]
    async fn accepted_factory_can_create_its_capability_after_shutdown_begins() {
        let execution = ExecutionBinding::external(tokio::runtime::Handle::current());
        let queue = LogQueue::new(
            LogQueueConfig::default(),
            reqwest::Client::new(),
            execution.clone(),
        );
        let lifecycle = Lifecycle::new(execution.clone(), queue.clone());
        let (entered, arrival) = std::sync::mpsc::channel();
        let (resume, waiting) = std::sync::mpsc::channel();
        let factory = std::thread::spawn(move || {
            execution.response_try::<(), BraintrustError, _, _>(|| {
                entered.send(()).unwrap();
                waiting.recv().unwrap();
                let operation = queue.operation()?;
                Ok(async move {
                    let write = operation.submitter.submit_deferred(
                        "late".into(),
                        Box::pin(async {
                            Err(BraintrustError::InvalidConfig(
                                "late preparation failed".into(),
                            ))
                        }),
                    );
                    assert!(write.submission().is_ok());
                    assert!(matches!(write.await, Err(WriteError::Delivery(_))));
                    drop(operation);
                    Ok(())
                })
            })
        });
        arrival.recv_timeout(Duration::from_secs(5)).unwrap();
        let shutdown = lifecycle.shutdown(Duration::from_secs(5));
        resume.send(()).unwrap();
        let response = factory.join().unwrap();
        response.await.unwrap();
        assert!(matches!(shutdown.await, Err(ShutdownError::Delivery(_))));
    }
}
