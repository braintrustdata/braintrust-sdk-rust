//! Experiment support for tracking logged events across model versions.
//!
//! Experiments allow you to:
//! - Log evaluation events with input, output, expected, and scores
//! - Compare results against baseline experiments
//! - Get aggregated summaries of experiment performance
//!
//! # Example
//!
//! ```ignore
//! let experiment = client
//!     .experiment_builder_with_credentials(&api_key, &org_id)
//!     .project_name("my-project")
//!     .experiment_name("gpt-4-baseline")
//!     .build();
//!
//! let id = experiment.log(
//!     ExperimentLog::builder()
//!         .input(json!({"question": "What is 2+2?"}))
//!         .output(json!({"answer": "4"}))
//!         .expected(json!({"answer": "4"}))
//!         .score("accuracy", 1.0)
//!         .build()?
//! ).await;
//!
//! experiment.flush().await?;
//! ```

// Internal API module (not public)
pub(crate) mod api;

// Public modules
mod builder;
mod event_log;
mod experiment;
mod feedback;
mod metadata;
mod span_builder;
mod summary;

// Public re-exports
pub use builder::{ExperimentBuilder, ExperimentBuilderError};
pub use event_log::{ExperimentLog, ExperimentLogBuilder};
pub use experiment::Experiment;
pub use feedback::{Feedback, FeedbackBuilder, FeedbackBuilderError};
pub use metadata::{
    BaseExperimentInfo, ExportedExperiment, GitMetadataCollect, GitMetadataField,
    GitMetadataSettings, ProjectMetadata, RepoInfo,
};
pub use span_builder::ExperimentSpanBuilder;
pub use summary::{ExperimentSummary, MetricSummary, ScoreSummary};
