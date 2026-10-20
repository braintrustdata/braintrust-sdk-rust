use serde::{ser::SerializeStruct, Serialize, Serializer};

/// A computation failure retains its stage and original application error.
/// Other scorers still run after a scorer failure; task failures skip scoring.
#[derive(Debug, thiserror::Error)]
pub enum CaseError {
    #[error("task failed: {source}")]
    Task {
        #[source]
        source: anyhow::Error,
    },
    #[error("scorer {name} failed: {source}")]
    Scorer {
        name: String,
        #[source]
        source: anyhow::Error,
    },
}
impl Serialize for CaseError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct(
            "CaseError",
            match self {
                Self::Task { .. } => 2,
                Self::Scorer { .. } => 3,
            },
        )?;
        let (stage, source) = match self {
            Self::Task { source } => ("task", source),
            Self::Scorer { name, source } => {
                state.serialize_field("name", name)?;
                ("scorer", source)
            }
        };
        state.serialize_field("stage", stage)?;
        state.serialize_field("message", &source.to_string())?;
        state.end()
    }
}

/// Invalid evaluation definitions are rejected before source access or recording.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalConfigError {
    #[error("evaluation concurrency must be greater than zero")]
    ZeroConcurrency,
    #[error("scorer at index {index} must have a nonblank name")]
    EmptyScorerName { index: usize },
    #[error("duplicate scorer name: {name}")]
    DuplicateScorerName { name: String },
}

/// Run-level failures retain their boundary and source location.
/// Case indices are zero-based; None denotes run-level recording work.
#[derive(Debug, thiserror::Error)]
pub enum EvalRunFailure {
    #[error(transparent)]
    Config(#[from] EvalConfigError),
    #[error("case source failed at index {case_index}: {source}")]
    Source {
        case_index: usize,
        #[source]
        source: super::SourceError,
    },
    #[error("recording failed for case {case_index:?}: {source}")]
    Recording {
        case_index: Option<usize>,
        #[source]
        source: crate::BraintrustError,
    },
    #[error(transparent)]
    Execution(#[from] crate::ExecutionError),
    #[error(transparent)]
    Admission(#[from] crate::EnqueueError),
}

/// An incomplete run preserves finished case results and every run-level failure.
/// Case computation errors remain in those results rather than aborting the run.
pub struct EvalRunError<I, O> {
    pub failures: Vec<EvalRunFailure>,
    pub partial: Option<Box<super::EvalSummary<I, O>>>,
}
impl<I, O> EvalRunError<I, O> {
    fn new(failure: EvalRunFailure) -> Self {
        Self {
            failures: vec![failure],
            partial: None,
        }
    }
}
impl<I, O> std::fmt::Debug for EvalRunError<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalRunError")
            .field("failures", &self.failures)
            .field(
                "completed_cases",
                &self.partial.as_ref().map(|summary| summary.results.len()),
            )
            .finish()
    }
}
impl<I, O> std::fmt::Display for EvalRunError<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(first) = self.failures.first() {
            write!(f, "{first}")?;
        }
        if self.failures.len() > 1 {
            write!(f, " ({} run failures)", self.failures.len())?;
        }
        Ok(())
    }
}
impl<I, O> std::error::Error for EvalRunError<I, O> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures
            .first()
            .map(|failure| failure as &dyn std::error::Error)
    }
}
impl<I, O> From<EvalConfigError> for EvalRunError<I, O> {
    fn from(error: EvalConfigError) -> Self {
        Self::new(EvalRunFailure::Config(error))
    }
}
impl<I, O> From<crate::BraintrustError> for EvalRunError<I, O> {
    fn from(source: crate::BraintrustError) -> Self {
        Self::new(EvalRunFailure::Recording {
            case_index: None,
            source,
        })
    }
}
impl<I, O> From<crate::ExecutionError> for EvalRunError<I, O> {
    fn from(error: crate::ExecutionError) -> Self {
        Self::new(EvalRunFailure::Execution(error))
    }
}
impl<I, O> From<crate::EnqueueError> for EvalRunError<I, O> {
    fn from(error: crate::EnqueueError) -> Self {
        Self::new(EvalRunFailure::Admission(error))
    }
}
