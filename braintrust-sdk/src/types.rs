//! Core data types for the Braintrust SDK

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// Type of span for tracing different operations
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpanType {
    /// A task span represents a top-level operation
    Task,
    /// An LLM span represents a language model call
    Llm,
    /// A function span represents a function call
    Function,
    /// An eval span represents an evaluation
    Eval,
    /// A score span represents a scoring operation
    Score,
    /// A tool span represents a tool call
    Tool,
}

impl Default for SpanType {
    fn default() -> Self {
        SpanType::Task
    }
}

/// Span attributes provide metadata about the span
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpanAttributes {
    /// Name of the span
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Type of the span
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub span_type: Option<SpanType>,
}

/// Metrics associated with a span
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpanMetrics {
    /// Start time (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,

    /// End time (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,

    /// Number of prompt tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,

    /// Number of completion tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,

    /// Total number of tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,

    /// Estimated cost
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
}

/// Context information for a span
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpanContext {
    /// Filename of the caller
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_filename: Option<String>,

    /// Function name of the caller
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_functionname: Option<String>,

    /// Line number of the caller
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_lineno: Option<u32>,
}

/// A span event represents a single logged event in the Braintrust system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    /// Unique identifier for this event (row ID)
    pub id: String,

    /// Span ID for this event
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,

    /// Root span ID for the trace
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_span_id: Option<String>,

    /// Parent span IDs (for multi-parent scenarios)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_parents: Option<Vec<String>>,

    /// Project ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Log ID (literal "g" for project logs, "x" for playground logs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_id: Option<String>,

    /// Experiment ID (if this is part of an experiment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,

    /// Input data for this span
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,

    /// Output data for this span
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,

    /// Expected output (for evaluations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,

    /// Error information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Scores associated with this span
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<HashMap<String, f64>>,

    /// Metadata for this span
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,

    /// Metrics for this span
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<SpanMetrics>,

    /// Span attributes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_attributes: Option<SpanAttributes>,

    /// Context information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<SpanContext>,

    /// Creation timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,

    /// Internal flag for merging behavior (not sent to API)
    #[serde(rename = "_is_merge", skip_serializing_if = "Option::is_none")]
    pub is_merge: Option<bool>,
}

impl SpanEvent {
    /// Create a new span event with a unique ID
    pub fn new() -> Self {
        SpanEvent {
            id: Uuid::new_v4().to_string(),
            span_id: None,
            root_span_id: None,
            span_parents: None,
            project_id: None,
            log_id: None,
            experiment_id: None,
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            span_attributes: None,
            context: None,
            created: Some(Utc::now()),
            is_merge: None,
        }
    }

    /// Deep merge this event with another event
    /// First event (is_merge=false) replaces, subsequent events (is_merge=true) merge
    pub fn merge(&mut self, other: &SpanEvent) {
        // If this is not a merge operation, replace all fields
        if other.is_merge != Some(true) {
            *self = other.clone();
            return;
        }

        // Merge operation - merge field by field
        if other.span_id.is_some() {
            self.span_id = other.span_id.clone();
        }
        if other.root_span_id.is_some() {
            self.root_span_id = other.root_span_id.clone();
        }
        if other.span_parents.is_some() {
            self.span_parents = other.span_parents.clone();
        }
        if other.project_id.is_some() {
            self.project_id = other.project_id.clone();
        }
        if other.log_id.is_some() {
            self.log_id = other.log_id.clone();
        }
        if other.experiment_id.is_some() {
            self.experiment_id = other.experiment_id.clone();
        }
        if other.input.is_some() {
            self.input = merge_json_values(self.input.as_ref(), other.input.as_ref());
        }
        if other.output.is_some() {
            self.output = merge_json_values(self.output.as_ref(), other.output.as_ref());
        }
        if other.expected.is_some() {
            self.expected = merge_json_values(self.expected.as_ref(), other.expected.as_ref());
        }
        if other.error.is_some() {
            self.error = other.error.clone();
        }
        if other.scores.is_some() {
            if let Some(ref mut self_scores) = self.scores {
                if let Some(ref other_scores) = other.scores {
                    self_scores.extend(other_scores.clone());
                }
            } else {
                self.scores = other.scores.clone();
            }
        }
        if other.metadata.is_some() {
            self.metadata = merge_json_values(self.metadata.as_ref(), other.metadata.as_ref());
        }
        if other.metrics.is_some() {
            self.metrics = other.metrics.clone();
        }
        if other.span_attributes.is_some() {
            self.span_attributes = other.span_attributes.clone();
        }
        if other.context.is_some() {
            self.context = other.context.clone();
        }
        // Don't merge created timestamp
    }
}

impl Default for SpanEvent {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper function to merge JSON values (deep merge for objects)
fn merge_json_values(base: Option<&Value>, new: Option<&Value>) -> Option<Value> {
    match (base, new) {
        (None, None) => None,
        (Some(v), None) => Some(v.clone()),
        (None, Some(v)) => Some(v.clone()),
        (Some(Value::Object(base_map)), Some(Value::Object(new_map))) => {
            let mut merged = base_map.clone();
            for (key, value) in new_map {
                if let Some(base_value) = merged.get(key) {
                    merged.insert(key.clone(), merge_json_values(Some(base_value), Some(value)).unwrap());
                } else {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Some(Value::Object(merged))
        }
        (_, Some(v)) => Some(v.clone()),
    }
}

/// Parent span information for establishing span hierarchy
#[derive(Debug, Clone)]
pub struct ParentSpanIds {
    /// The parent span's span_id
    pub span_id: String,
    /// The parent span's root_span_id (for proper trace nesting)
    pub root_span_id: String,
}

/// Options for starting a new span
#[derive(Debug, Clone, Default)]
pub struct StartSpanOptions {
    /// Name of the span
    pub name: Option<String>,

    /// Type of the span
    pub span_type: Option<SpanType>,

    /// Parent span information (for establishing span hierarchy)
    pub parent: Option<ParentSpanIds>,

    /// Initial event data
    pub event: Option<SpanEvent>,
}

impl StartSpanOptions {
    /// Create new span options
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the span name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the span type
    pub fn with_type(mut self, span_type: SpanType) -> Self {
        self.span_type = Some(span_type);
        self
    }

    /// Set the parent span (extracts span_id and root_span_id for proper hierarchy)
    pub fn with_parent(mut self, parent: &std::sync::Arc<dyn crate::span::Span>) -> Self {
        self.parent = Some(ParentSpanIds {
            span_id: parent.span_id(),
            root_span_id: parent.root_span_id(),
        });
        self
    }
}
