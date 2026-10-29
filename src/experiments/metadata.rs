//! Metadata and configuration types for experiments.

use serde::{Deserialize, Serialize};

/// Metadata about the project containing an experiment.
///
/// Read values through [`Self::id`] and [`Self::name`].
///
/// ```compile_fail
/// use braintrust_sdk_rust::ProjectMetadata;
/// fn rename(project: &mut ProjectMetadata) { project.name = "other".into(); }
/// ```
#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    /// The project ID.
    pub(crate) id: String,
    /// The project name.
    pub(crate) name: String,
}

/// Base experiment identifier for comparison.
///
/// ```compile_fail
/// use braintrust_sdk_rust::BaseExperimentInfo;
/// fn rename(base: &mut BaseExperimentInfo) { base.name = "other".into(); }
/// ```
#[derive(Debug, Clone)]
pub struct BaseExperimentInfo {
    /// The base experiment ID.
    pub(crate) id: String,
    /// The base experiment name.
    pub(crate) name: String,
}

/// Exported experiment context for cross-process tracing.
///
/// Use this to pass experiment context to another process or service.
///
/// ```
/// use braintrust_sdk_rust::ExportedExperiment;
/// let context: ExportedExperiment = serde_json::from_str(
///     r#"{"object_type":"experiment","object_id":"test-experiment"}"#,
/// ).unwrap();
/// assert_eq!(context.object_type(), "experiment");
/// assert_eq!(context.object_id(), "test-experiment");
/// ```
///
/// ```compile_fail
/// use braintrust_sdk_rust::ExportedExperiment;
/// fn retarget(context: &mut ExportedExperiment) { context.object_id = "other".into(); }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedExperiment {
    /// The object type (always "experiment").
    pub(crate) object_type: String,
    /// The experiment ID.
    pub(crate) object_id: String,
}

pub use crate::api::experiments::RepoInfo;

impl ProjectMetadata {
    /// Return the project ID.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the project name.
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl BaseExperimentInfo {
    /// Return the base experiment ID.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the base experiment name.
    pub fn name(&self) -> &str {
        &self.name
    }
}
impl ExportedExperiment {
    /// Return the exported object type.
    pub fn object_type(&self) -> &str {
        &self.object_type
    }
    /// Return the experiment ID.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }
}
