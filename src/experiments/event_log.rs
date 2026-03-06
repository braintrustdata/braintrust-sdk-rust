//! Event data for experiment logging.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::span::SpanLog;

/// Event data to log to an experiment. All fields are optional.
///
/// Use `ExperimentLog::builder()` to construct instances.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ExperimentLog {
    pub(crate) input: Option<Value>,
    pub(crate) output: Option<Value>,
    pub(crate) expected: Option<Value>,
    pub(crate) error: Option<Value>,
    pub(crate) scores: Option<HashMap<String, f64>>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) metrics: Option<HashMap<String, f64>>,
    pub(crate) tags: Option<Vec<String>>,
}

impl ExperimentLog {
    /// Create a new ExperimentLog builder.
    pub fn builder() -> ExperimentLogBuilder {
        ExperimentLogBuilder::new()
    }

    /// Convert to SpanLog for internal use.
    pub(crate) fn into_span_log(self) -> SpanLog {
        let mut builder = SpanLog::builder();

        if let Some(input) = self.input {
            builder = builder.input(input);
        }
        if let Some(output) = self.output {
            builder = builder.output(output);
        }
        if let Some(expected) = self.expected {
            builder = builder.expected(expected);
        }
        if let Some(error) = self.error {
            builder = builder.error(error);
        }
        if let Some(scores) = self.scores {
            builder = builder.scores(scores);
        }
        if let Some(metadata) = self.metadata {
            builder = builder.metadata(metadata);
        }
        if let Some(metrics) = self.metrics {
            builder = builder.metrics(metrics);
        }
        if let Some(tags) = self.tags {
            builder = builder.tags(tags);
        }

        builder.build().expect("SpanLog build should not fail")
    }
}

/// Builder for ExperimentLog.
#[derive(Clone, Default)]
pub struct ExperimentLogBuilder {
    inner: ExperimentLog,
}

impl ExperimentLogBuilder {
    /// Create a new ExperimentLogBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the input data.
    pub fn input(mut self, input: impl Into<Value>) -> Self {
        self.inner.input = Some(input.into());
        self
    }

    /// Set the output data.
    pub fn output(mut self, output: impl Into<Value>) -> Self {
        self.inner.output = Some(output.into());
        self
    }

    /// Set the expected output for comparison.
    pub fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.inner.expected = Some(expected.into());
        self
    }

    /// Set the error that occurred during execution.
    pub fn error(mut self, error: impl Into<Value>) -> Self {
        self.inner.error = Some(error.into());
        self
    }

    /// Set multiple scores at once.
    pub fn scores(mut self, scores: HashMap<String, f64>) -> Self {
        self.inner.scores = Some(scores);
        self
    }

    /// Add a single score (0-1 range recommended).
    pub fn score(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .scores
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.inner.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner
            .metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set the metrics map.
    pub fn metrics(mut self, metrics: HashMap<String, f64>) -> Self {
        self.inner.metrics = Some(metrics);
        self
    }

    /// Add a single metric.
    pub fn metric(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .metrics
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Add a single tag for categorization.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.inner
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Build the ExperimentLog.
    pub fn build(self) -> ExperimentLog {
        self.inner
    }
}
