//! Builder for creating experiments.

use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::experiments::experiment::{Experiment, ExperimentState};
use crate::experiments::metadata::RepoInfo;
use crate::internal::execution::ExecutionBinding;
use crate::internal::ports::{ExperimentApi, SpanSubmitter};
use crate::internal::session::CredentialScope;

/// Error type for ExperimentBuilder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExperimentBuilderError {
    /// Project name is required but was not provided.
    MissingProjectName,
}

impl fmt::Display for ExperimentBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProjectName => {
                write!(f, "project_name is required but was not provided")
            }
        }
    }
}

impl std::error::Error for ExperimentBuilderError {}

/// Builder for creating experiments with lazy registration.
///
/// Registration is shared by clones of the resulting handle. Explicit
/// credentials belong to the resource and do not mutate the client's session.
pub struct ExperimentBuilder {
    api: Arc<dyn ExperimentApi>,
    execution: ExecutionBinding,
    submitter: Arc<dyn SpanSubmitter>,
    scope: CredentialScope,
    project_name: Option<String>,
    experiment_name: Option<String>,
    description: Option<String>,
    base_experiment: Option<String>,
    metadata: Option<Map<String, Value>>,
    public: Option<bool>,
    update: Option<bool>,
    repo_info: Option<RepoInfo>,
}

impl Clone for ExperimentBuilder {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            execution: self.execution.clone(),
            submitter: self.submitter.clone(),
            scope: self.scope.clone(),
            project_name: self.project_name.clone(),
            experiment_name: self.experiment_name.clone(),
            description: self.description.clone(),
            base_experiment: self.base_experiment.clone(),
            metadata: self.metadata.clone(),
            public: self.public,
            update: self.update,
            repo_info: self.repo_info.clone(),
        }
    }
}

impl ExperimentBuilder {
    /// Create a new ExperimentBuilder (internal use).
    pub(crate) fn new(
        api: Arc<dyn ExperimentApi>,
        execution: ExecutionBinding,
        submitter: Arc<dyn SpanSubmitter>,
        scope: CredentialScope,
    ) -> Self {
        Self {
            api,
            execution,
            submitter,
            scope,
            project_name: None,
            experiment_name: None,
            description: None,
            base_experiment: None,
            metadata: None,
            public: None,
            update: None,
            repo_info: None,
        }
    }

    /// Set the organization name.
    pub fn org_name(mut self, org_name: impl Into<String>) -> Self {
        self.scope = self.scope.with_org_name(org_name.into());
        self
    }

    /// Set the project name (required).
    pub fn project_name(mut self, project_name: impl Into<String>) -> Self {
        self.project_name = Some(project_name.into());
        self
    }

    /// Set the experiment name. If not provided, a name will be auto-generated.
    pub fn experiment_name(mut self, experiment_name: impl Into<String>) -> Self {
        self.experiment_name = Some(experiment_name.into());
        self
    }

    /// Set the experiment description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the base experiment name for comparison.
    pub fn base_experiment(mut self, base_experiment: impl Into<String>) -> Self {
        self.base_experiment = Some(base_experiment.into());
        self
    }

    /// Set the experiment metadata.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set whether the experiment is public.
    pub fn public(mut self, public: bool) -> Self {
        self.public = Some(public);
        self
    }

    /// Continue logging to an existing experiment instead of creating a new one.
    ///
    /// When set to `true`, if an experiment with the same name already exists,
    /// logging will continue to that experiment. If `false` (the default),
    /// a new experiment will always be created.
    pub fn update(mut self, update: bool) -> Self {
        self.update = Some(update);
        self
    }

    /// Set explicit repository information.
    ///
    /// This overrides any automatic git metadata collection.
    pub fn repo_info(mut self, repo_info: RepoInfo) -> Self {
        self.repo_info = Some(repo_info);
        self
    }

    /// Build the experiment. Returns an error if required fields are missing.
    pub fn build(self) -> std::result::Result<Experiment, ExperimentBuilderError> {
        let project_name = self
            .project_name
            .ok_or(ExperimentBuilderError::MissingProjectName)?;

        Ok(Experiment {
            api: self.api,
            execution: self.execution,
            submitter: self.submitter,
            scope: self.scope,
            project_name,
            experiment_name: self.experiment_name,
            description: self.description,
            base_experiment: self.base_experiment,
            metadata: self.metadata,
            public: self.public,
            update: self.update,
            repo_info: self.repo_info,
            state: Arc::new(ExperimentState::default()),
        })
    }
}
