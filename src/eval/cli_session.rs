//! Bounded live CLI ordering; no retained per-run outcomes.
use super::{EvalRunError, EvalSummary, ReportError};
use crate::{Completion, EnqueueError, ExecutionError};
use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use std::{
    fmt,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

/// Failure to configure or initialize a CLI session.
#[derive(Debug, thiserror::Error)]
pub enum BtInitError {
    #[error(transparent)]
    Client(#[from] crate::BraintrustError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Admission(#[from] EnqueueError),
    #[error("invalid BT_EVAL_FILTER_PARSED: {0}")]
    Filters(#[source] serde_json::Error),
    #[error("invalid pattern in CLI filter {index}: {source}")]
    FilterPattern {
        index: usize,
        #[source]
        source: regex::Error,
    },
    #[error("failed to connect to CLI SSE {transport}: {source}")]
    Sse {
        transport: &'static str,
        #[source]
        source: std::io::Error,
    },
}

/// CLI run failure. Detailed computation/recording results remain available.
pub enum BtEvalError<I, O> {
    Run(EvalRunError<I, O>),
    CaseFailures {
        summary: Box<EvalSummary<I, O>>,
    },
    /// A previous evaluation failed with terminate-on-failure enabled.
    Stopped,
    /// CLI runs inherit their session/client binding, rather than a run override.
    RuntimeOverride,
}
impl<I, O> fmt::Debug for BtEvalError<I, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(error) => f.debug_tuple("Run").field(error).finish(),
            Self::CaseFailures { summary } => f
                .debug_struct("CaseFailures")
                .field("failed_cases", &summary.failed_cases)
                .finish(),
            Self::Stopped => f.write_str("Stopped"),
            Self::RuntimeOverride => f.write_str("RuntimeOverride"),
        }
    }
}
impl<I, O> fmt::Display for BtEvalError<I, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(error) => error.fmt(f),
            Self::CaseFailures { summary } => write!(f, "Evaluator '{}' failed with {} error(s)", summary.name, summary.failed_cases),
            Self::Stopped => f.write_str("CLI evaluation session stopped after a previous failure"),
            Self::RuntimeOverride => f.write_str("CLI evaluations inherit their session/client runtime; omit the run runtime override"),
        }
    }
}
impl<I: 'static, O: 'static> std::error::Error for BtEvalError<I, O> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Run(error) => Some(error),
            _ => None,
        }
    }
}
impl<I, O> From<EvalRunError<I, O>> for BtEvalError<I, O> {
    fn from(error: EvalRunError<I, O>) -> Self {
        Self::Run(error)
    }
}
impl<I, O> From<ExecutionError> for BtEvalError<I, O> {
    fn from(error: ExecutionError) -> Self {
        Self::Run(error.into())
    }
}
impl<I, O> From<EnqueueError> for BtEvalError<I, O> {
    fn from(error: EnqueueError) -> Self {
        Self::Run(error.into())
    }
}

/// Failure to join a CLI session or write its list output.
#[derive(Debug, thiserror::Error)]
pub enum BtFinishError {
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Output(#[from] ReportError),
}

type Barrier = Shared<BoxFuture<'static, ()>>;
#[derive(Clone)]
pub(crate) struct Session(Arc<Mutex<State>>);
struct State {
    all_passed: bool,
    stopped_after: Option<u64>,
    next: u64,
    terminate: bool,
    tail: Barrier,
}
impl Session {
    pub fn new(terminate: bool) -> Self {
        Self(Arc::new(Mutex::new(State {
            all_passed: true,
            stopped_after: None,
            next: 0,
            terminate,
            tail: futures::future::ready(()).boxed().shared(),
        })))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn all_passed(&self) -> bool {
        self.state().all_passed
    }
    pub fn stopped(&self) -> bool {
        self.state().stopped_after.is_some()
    }
    pub fn should_skip(&self, sequence: u64) -> bool {
        self.state()
            .stopped_after
            .is_some_and(|failed| sequence > failed)
    }
    pub fn fail(&self) {
        let sequence = self.state().next;
        self.fail_at(sequence);
    }
    pub fn fail_at(&self, sequence: u64) {
        let mut state = self.state();
        state.all_passed = false;
        if state.terminate {
            state.stopped_after = Some(
                state
                    .stopped_after
                    .map_or(sequence, |previous| previous.min(sequence)),
            );
        }
    }
    /// Called inside response admission, after acquiring any write operation.
    pub fn reserve<I, O>(&self) -> Result<(Barrier, Ticket), BtEvalError<I, O>> {
        let mut state = self.state();
        if state.stopped_after.is_some() {
            return Err(BtEvalError::Stopped);
        }
        let sequence = state.next;
        state.next = sequence
            .checked_add(1)
            .ok_or_else(|| BtEvalError::from(EnqueueError::Full))?;
        let previous = state.tail.clone();
        let (sender, receiver) = oneshot::channel();
        state.tail = async move {
            let _ = receiver.await;
        }
        .boxed()
        .shared();
        Ok((
            previous,
            Ticket {
                session: self.clone(),
                sender: Some(sender),
                completed: false,
                sequence,
            },
        ))
    }
    pub fn tail(&self) -> Barrier {
        self.state().tail.clone()
    }
}
pub(crate) struct Ticket {
    session: Session,
    sender: Option<oneshot::Sender<()>>,
    completed: bool,
    sequence: u64,
}
impl Ticket {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn complete(mut self, failed: bool) {
        if failed {
            self.session.fail_at(self.sequence);
        }
        self.completed = true;
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.completed {
            self.session.fail_at(self.sequence);
        }
        // Unblock successors even on panic, timeout, or loss before first poll.
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(());
        }
    }
}
pub(crate) fn ready<T, E>(value: T) -> Completion<T, E> {
    let (sender, completion) = Completion::pair();
    let _ = sender.send(Ok(value));
    completion
}
