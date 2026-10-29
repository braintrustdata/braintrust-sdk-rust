//! Builder for creating spans within experiments.

use crate::experiments::experiment::Experiment;
use crate::span::{SpanBuilder, SpanHandle};
use crate::types::{ParentSpanInfo, SpanType};

/// Builder for creating spans within an experiment.
#[derive(Clone)]
pub struct ExperimentSpanBuilder {
    inner: SpanBuilder,
}

impl ExperimentSpanBuilder {
    pub(crate) fn new(experiment: &Experiment, experiment_id: &str) -> Self {
        Self {
            inner: SpanBuilder::new(experiment.submitter.clone(), experiment.scope.clone())
                .parent_info(ParentSpanInfo::Experiment {
                    object_id: experiment_id.to_owned(),
                })
                .span_type(SpanType::Eval),
        }
    }

    /// Set the span name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.name(name);
        self
    }

    /// Set the span type.
    pub fn span_type(mut self, span_type: SpanType) -> Self {
        self.inner = self.inner.span_type(span_type);
        self
    }

    /// Build the span.
    pub fn build(self) -> SpanHandle {
        self.inner.build()
    }
}
