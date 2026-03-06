//! Metadata and configuration types for experiments.

use serde::{Deserialize, Serialize};

/// Metadata about the project containing an experiment.
#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    /// The project ID.
    pub id: String,
    /// The project name.
    pub name: String,
}

/// Base experiment identifier for comparison.
#[derive(Debug, Clone)]
pub struct BaseExperimentInfo {
    /// The base experiment ID.
    pub id: String,
    /// The base experiment name.
    pub name: String,
}

/// Exported experiment context for cross-process tracing.
///
/// Use this to pass experiment context to another process or service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedExperiment {
    /// The object type (always "experiment").
    pub object_type: String,
    /// The experiment ID.
    pub object_id: String,
}

/// Settings for git metadata collection.
#[derive(Debug, Clone, Default)]
pub struct GitMetadataSettings {
    /// How much git metadata to collect.
    pub collect: GitMetadataCollect,
    /// Specific fields to collect (when collect is `Some`).
    pub fields: Option<Vec<GitMetadataField>>,
}

/// How much git metadata to collect.
#[derive(Debug, Clone, Default)]
pub enum GitMetadataCollect {
    /// Collect all available git metadata.
    #[default]
    All,
    /// Don't collect any git metadata.
    None,
    /// Collect only specific fields.
    Some,
}

/// Individual git metadata fields that can be collected.
#[derive(Debug, Clone)]
pub enum GitMetadataField {
    /// Git commit SHA.
    Commit,
    /// Current branch name.
    Branch,
    /// Current tag (if any).
    Tag,
    /// Whether the working directory has uncommitted changes.
    Dirty,
    /// Git author name.
    AuthorName,
    /// Git author email.
    AuthorEmail,
    /// Git commit message.
    CommitMessage,
    /// Git commit timestamp.
    CommitTime,
}

/// Repository information for experiment tracking.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoInfo {
    /// Git commit SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Current branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Current tag (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Whether the working directory has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    /// Git author name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    /// Git author email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
    /// Git commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    /// Git commit timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_time: Option<String>,
}
