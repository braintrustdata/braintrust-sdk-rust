use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{Config, Error, Transport};
use crate::internal::{
    execution::ExecutionBinding,
    shared_outcome::{SharedOutcome, SharedResponder},
};
use crate::{Completion, ExecutionError, ShutdownError};

/// Eager one-attempt API operations sharing one execution binding and lifecycle.
/// Clones and endpoint views share admission and shutdown state. Construction
/// validates local configuration and starts the selected binding without login.
#[derive(Clone)]
pub struct Client {
    transport: Transport,
    execution: ExecutionBinding,
    shutdown: Arc<Mutex<Option<SharedOutcome<(), ShutdownError>>>>,
}
impl Client {
    pub fn new(config: Config) -> Result<Self, Error> {
        let runtime = config.runtime.clone();
        let transport = Transport::new(config)?;
        let execution = ExecutionBinding::new(runtime).map_err(|error| match error {
            crate::BraintrustError::Execution(error) => Error::Execution(error),
            error => Error::Config(error.to_string()),
        })?;
        Ok(Self {
            transport,
            execution,
            shutdown: Arc::new(Mutex::new(None)),
        })
    }
    pub(crate) fn submit<T, F, W>(&self, make: F) -> Completion<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Transport) -> W,
        W: Future<Output = Result<T, Error>> + Send + 'static,
    {
        self.execution
            .response_try(|| Ok(make(self.transport.clone())))
    }
    pub fn logs3(&self) -> super::logs3::Logs3 {
        super::logs3::Logs3 {
            client: self.clone(),
        }
    }
    pub fn btql(&self) -> super::btql::Btql {
        super::btql::Btql {
            client: self.clone(),
        }
    }
    pub fn datasets(&self) -> super::datasets::Datasets {
        super::datasets::Datasets {
            client: self.clone(),
        }
    }
    pub fn experiments(&self) -> super::experiments::Experiments {
        super::experiments::Experiments {
            client: self.clone(),
        }
    }
    pub fn projects(&self) -> super::projects::Projects {
        super::projects::Projects {
            client: self.clone(),
        }
    }
    /// Submit one login request without changing this client's immutable scope.
    pub fn login(&self) -> Completion<super::login::LoginResponse, Error> {
        self.submit(|transport| async move { transport.login().await })
    }
    /// Close admission immediately and wait for accepted requests under one
    /// deadline. Repeated calls observe the first deadline and outcome. Request
    /// failures remain on their own tokens and are not a shutdown failure log.
    pub fn shutdown(&self, timeout: Duration) -> Completion<(), ShutdownError> {
        let mut state = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(outcome) = &*state {
            return outcome.observe();
        }
        let outcome = SharedOutcome::default();
        let (sender, completion) = Completion::pair();
        let responder = SharedResponder::new(sender, Some(outcome.clone()));
        *state = Some(outcome);
        self.execution.close_responses();
        let execution = self.execution.clone();
        let deadline = tokio::time::Instant::now().checked_add(timeout);
        self.execution.spawn(async move {
            let work = execution.responses_drained();
            let result = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, work).await,
                None => Ok(work.await),
            };
            let result = match result {
                Ok(result) => result.map_err(ShutdownError::Execution),
                Err(_) => {
                    execution.cancel_responses(ExecutionError::TimedOut);
                    Err(ShutdownError::Execution(ExecutionError::TimedOut))
                }
            };
            let _ = responder.send(result);
        });
        completion
    }
}
