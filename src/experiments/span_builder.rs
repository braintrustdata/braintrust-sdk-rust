//! Builder for creating spans within experiments.

use std::sync::Arc;

use crate::experiments::experiment::{Experiment, ExperimentMetadata};
use crate::internal::{ports::SpanSubmitter, session::CredentialScope};
use crate::span::{SpanBuilder, SpanHandle};
use crate::types::{ParentSpanInfo, SpanType};

/// Builder for creating spans within an experiment.
pub struct ExperimentSpanBuilder {
    submitter: Arc<dyn SpanSubmitter>,
    scope: CredentialScope,
    metadata: ExperimentMetadata,
    name: Option<String>,
    span_type: SpanType,
}

impl ExperimentSpanBuilder {
    pub(crate) fn new(experiment: &Experiment, metadata: ExperimentMetadata) -> Self {
        Self {
            submitter: experiment.submitter.clone(),
            scope: experiment.scope.clone(),
            metadata,
            name: None,
            span_type: SpanType::Eval,
        }
    }

    /// Set the span name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the span type.
    pub fn span_type(mut self, span_type: SpanType) -> Self {
        self.span_type = span_type;
        self
    }

    /// Build the span.
    pub fn build(self) -> SpanHandle {
        let parent_info = ParentSpanInfo::Experiment {
            object_id: self.metadata.experiment_id.clone(),
        };

        // Note: When using ParentSpanInfo::Experiment, we don't need project_name
        // because the destination is determined by the experiment_id.
        let mut builder = SpanBuilder::new(self.submitter, self.scope)
            .parent_info(parent_info)
            .span_type(self.span_type);

        if let Some(name) = self.name {
            builder = builder.name(name);
        }
        builder.build()
    }
}
