//! Builder for creating spans within experiments.

use std::sync::Arc;

use crate::experiments::experiment::{Experiment, ExperimentMetadata};
use crate::span::{SpanBuilder, SpanHandle, SpanSubmitter};
use crate::types::{ParentSpanInfo, SpanType};

/// Builder for creating spans within an experiment.
#[allow(private_bounds)]
pub struct ExperimentSpanBuilder<'a, S: SpanSubmitter> {
    experiment: &'a Experiment<S>,
    metadata: ExperimentMetadata,
    name: Option<String>,
    span_type: SpanType,
}

#[allow(private_bounds)]
impl<'a, S: SpanSubmitter + 'static> ExperimentSpanBuilder<'a, S> {
    pub(crate) fn new(experiment: &'a Experiment<S>, metadata: ExperimentMetadata) -> Self {
        Self {
            experiment,
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
    pub fn build(self) -> SpanHandle<S> {
        let parent_info = ParentSpanInfo::Experiment {
            object_id: self.metadata.experiment_id.clone(),
        };

        // Note: When using ParentSpanInfo::Experiment, we don't need project_name
        // because the destination is determined by the experiment_id.
        let mut builder = SpanBuilder::new(
            Arc::clone(&self.experiment.submitter),
            self.experiment.token.clone(),
            self.experiment.org_id.clone(),
        )
        .parent_info(parent_info)
        .span_type(self.span_type);

        if let Some(org_name) = &self.experiment.org_name {
            builder = builder.org_name(org_name.clone());
        }

        builder.build()
    }
}
