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

/// Repository information for experiment tracking.
#[derive(bon::Builder, Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct RepoInfo {
    /// Git commit SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) commit: Option<String>,
    /// Current branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) branch: Option<String>,
    /// Current tag (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) tag: Option<String>,
    /// Whether the working directory has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dirty: Option<bool>,
    /// Git author name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) author_name: Option<String>,
    /// Git author email.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) author_email: Option<String>,
    /// Git commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) commit_message: Option<String>,
    /// Git commit timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) commit_time: Option<String>,
}

impl From<RepoInfo> for crate::api::ExperimentRepoInfo {
    fn from(value: RepoInfo) -> Self {
        let mut result = Self::default();
        result.commit = value.commit;
        result.branch = value.branch;
        result.tag = value.tag;
        result.dirty = value.dirty;
        result.author_name = value.author_name;
        result.author_email = value.author_email;
        result.commit_message = value.commit_message;
        result.commit_time = value.commit_time;
        result
    }
}

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

impl RepoInfo {
    /// Return the commit.
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }
    /// Return the branch.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    /// Return the tag.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
    /// Return the dirty.
    pub fn dirty(&self) -> Option<&bool> {
        self.dirty.as_ref()
    }
    /// Return the author name.
    pub fn author_name(&self) -> Option<&str> {
        self.author_name.as_deref()
    }
    /// Return the author email.
    pub fn author_email(&self) -> Option<&str> {
        self.author_email.as_deref()
    }
    /// Return the commit message.
    pub fn commit_message(&self) -> Option<&str> {
        self.commit_message.as_deref()
    }
    /// Return the commit time.
    pub fn commit_time(&self) -> Option<&str> {
        self.commit_time.as_deref()
    }
}
