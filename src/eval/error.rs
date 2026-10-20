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
