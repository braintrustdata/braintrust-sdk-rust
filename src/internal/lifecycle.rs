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
        let writes = self.writes.shutdown(timeout);
        let execution = self.execution.clone();
        let queue = self.writes.clone();
        let deadline = tokio::time::Instant::now().checked_add(timeout);
        self.execution.spawn(async move {
            let work = async {
                // A write failure must not stop waiting for accepted HTTP work.
                let (writes, responses) =
                    futures::join!(async { writes.await }, execution.responses_drained());
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
