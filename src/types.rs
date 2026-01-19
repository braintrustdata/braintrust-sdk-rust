use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::fmt;

pub const LOGS_API_VERSION: u8 = 2;

/// The type of span object, serialized as its integer representation for wire compatibility.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq, Eq)]
#[repr(u8)]
pub enum SpanObjectType {
    Experiment = 1,
    ProjectLogs = 2,
    PlaygroundLogs = 3,
}

impl SpanObjectType {
    pub fn as_str(self) -> &'static str {
        match self {
            SpanObjectType::Experiment => "experiment",
            SpanObjectType::ProjectLogs => "project_logs",
            SpanObjectType::PlaygroundLogs => "playground_logs",
        }
    }
}

impl fmt::Display for SpanObjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<u8> for SpanObjectType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(SpanObjectType::Experiment),
            2 => Ok(SpanObjectType::ProjectLogs),
            3 => Ok(SpanObjectType::PlaygroundLogs),
            _ => Err(()),
        }
    }
}

/// The type of span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanType {
    #[default]
    Llm,
    Score,
    Function,
    Eval,
    Task,
    Tool,
    Automation,
    Facet,
    Preprocessor,
}

/// Span attributes with typed known fields and passthrough for extras.
///
/// Uses `#[serde(flatten)]` to serialize/deserialize unknown fields into the
/// `extra` map, matching the TS SDK's `.passthrough()` behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SpanAttributes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub span_type: Option<SpanType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Catch-all for additional fields (passthrough behavior).
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

/// The destination for a log row. Each variant represents a mutually exclusive
/// target: an experiment, project logs, or playground logs.
///
/// NOTE: The `untagged` attribute is only safe if the field sets are also
/// mutually exclusive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum LogDestination {
    /// Log to an experiment (for evaluation runs).
    Experiment { experiment_id: String },
    /// Log to project logs (general observability).
    ProjectLogs { project_id: String, log_id: String },
    /// Log to playground logs (interactive sessions).
    PlaygroundLogs {
        prompt_session_id: String,
        log_id: String,
    },
}

impl LogDestination {
    /// Create a new experiment destination.
    pub fn experiment(experiment_id: impl Into<String>) -> Self {
        Self::Experiment {
            experiment_id: experiment_id.into(),
        }
    }

    /// Create a new project logs destination.
    pub fn project_logs(project_id: impl Into<String>) -> Self {
        Self::ProjectLogs {
            project_id: project_id.into(),
            log_id: "g".to_string(),
        }
    }

    /// Create a new playground logs destination.
    pub fn playground_logs(prompt_session_id: impl Into<String>) -> Self {
        Self::PlaygroundLogs {
            prompt_session_id: prompt_session_id.into(),
            log_id: "x".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Logs3Request {
    pub rows: Vec<Logs3Row>,
    pub api_version: u8,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Logs3Row {
    pub id: String,
    #[serde(rename = "_is_merge", skip_serializing_if = "Option::is_none")]
    pub is_merge: Option<bool>,
    pub span_id: String,
    pub root_span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_parents: Option<Vec<String>>,
    #[serde(flatten)]
    pub destination: LogDestination,
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<HashMap<String, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_attributes: Option<SpanAttributes>,
    pub created: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct SpanPayload {
    pub row_id: String,
    pub span_id: String,
    pub is_merge: bool,
    pub org_id: String,
    pub org_name: Option<String>,
    pub project_name: Option<String>,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub metadata: Option<Map<String, Value>>,
    pub metrics: Option<HashMap<String, f64>>,
    pub span_attributes: Option<SpanAttributes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParentSpanInfo {
    Experiment {
        object_id: String,
    },
    ProjectLogs {
        object_id: String,
    },
    ProjectName {
        project_name: String,
    },
    PlaygroundLogs {
        object_id: String,
    },
    FullSpan {
        object_type: SpanObjectType,
        object_id: String,
        span_id: String,
        root_span_id: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    pub audio_tokens: Option<u32>,
    pub cached_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionTokensDetails {
    pub audio_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub accepted_prediction_tokens: Option<u32>,
    pub rejected_prediction_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct UsageMetrics {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub prompt_cached_tokens: Option<u32>,
    pub prompt_cache_creation_tokens: Option<u32>,
    pub completion_reasoning_tokens: Option<u32>,
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

/// Usage statistics that can deserialize from both OpenAI and Anthropic formats.
///
/// OpenAI uses `prompt_tokens`/`completion_tokens`, while Anthropic uses
/// `input_tokens`/`output_tokens`. The serde aliases allow this struct to
/// deserialize from either format automatically.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default, alias = "input_tokens")]
    pub prompt_tokens: u32,
    #[serde(default, alias = "output_tokens")]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "cache_read_input_tokens"
    )]
    pub prompt_cached_tokens: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "cache_creation_input_tokens"
    )]
    pub prompt_cache_creation_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

impl Usage {
    /// Create a Usage from UsageMetrics, returning None if no metrics are present.
    pub fn from_metrics(metrics: UsageMetrics) -> Option<Self> {
        let has_metrics = metrics.prompt_tokens.is_some()
            || metrics.completion_tokens.is_some()
            || metrics.total_tokens.is_some()
            || metrics.reasoning_tokens.is_some()
            || metrics.prompt_cached_tokens.is_some()
            || metrics.prompt_cache_creation_tokens.is_some()
            || metrics.completion_reasoning_tokens.is_some();

        if !has_metrics {
            return None;
        }

        let prompt = metrics.prompt_tokens.unwrap_or_default();
        let completion = metrics.completion_tokens.unwrap_or_default();
        let total = metrics
            .total_tokens
            .or_else(|| {
                if prompt != 0 || completion != 0 {
                    Some(prompt + completion)
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let prompt_details = metrics.prompt_tokens_details.clone();
        let completion_details = metrics.completion_tokens_details.clone();

        Some(Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            reasoning_tokens: metrics.reasoning_tokens,
            prompt_cached_tokens: metrics.prompt_cached_tokens.or_else(|| {
                prompt_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens)
            }),
            prompt_cache_creation_tokens: metrics.prompt_cache_creation_tokens.or_else(|| {
                prompt_details
                    .as_ref()
                    .and_then(|details| details.cache_creation_tokens)
            }),
            completion_reasoning_tokens: metrics.completion_reasoning_tokens.or_else(|| {
                completion_details
                    .as_ref()
                    .and_then(|details| details.reasoning_tokens)
            }),
            prompt_tokens_details: prompt_details,
            completion_tokens_details: completion_details,
        })
    }
}

pub fn usage_metrics_to_map(usage: UsageMetrics) -> HashMap<String, f64> {
    let mut metrics = HashMap::new();
    insert_metric(&mut metrics, "prompt_tokens", usage.prompt_tokens);
    insert_metric(&mut metrics, "completion_tokens", usage.completion_tokens);
    insert_metric(&mut metrics, "tokens", usage.total_tokens);
    insert_metric(&mut metrics, "reasoning_tokens", usage.reasoning_tokens);
    insert_metric(
        &mut metrics,
        "completion_reasoning_tokens",
        usage.completion_reasoning_tokens,
    );
    insert_metric(
        &mut metrics,
        "prompt_cached_tokens",
        usage.prompt_cached_tokens,
    );
    insert_metric(
        &mut metrics,
        "prompt_cache_creation_tokens",
        usage.prompt_cache_creation_tokens,
    );

    if let Some(details) = usage.prompt_tokens_details {
        insert_metric(&mut metrics, "prompt_audio_tokens", details.audio_tokens);
        if usage.prompt_cached_tokens.is_none() {
            insert_metric(&mut metrics, "prompt_cached_tokens", details.cached_tokens);
        }
        if usage.prompt_cache_creation_tokens.is_none() {
            insert_metric(
                &mut metrics,
                "prompt_cache_creation_tokens",
                details.cache_creation_tokens,
            );
        }
    }

    if let Some(details) = usage.completion_tokens_details {
        insert_metric(
            &mut metrics,
            "completion_audio_tokens",
            details.audio_tokens,
        );
        if usage.completion_reasoning_tokens.is_none() {
            insert_metric(
                &mut metrics,
                "completion_reasoning_tokens",
                details.reasoning_tokens,
            );
        }
        insert_metric(
            &mut metrics,
            "completion_accepted_prediction_tokens",
            details.accepted_prediction_tokens,
        );
        insert_metric(
            &mut metrics,
            "completion_rejected_prediction_tokens",
            details.rejected_prediction_tokens,
        );
    }

    metrics
}

fn insert_metric(metrics: &mut HashMap<String, f64>, key: &str, value: Option<u32>) {
    if let Some(value) = value {
        metrics.insert(key.to_string(), value as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn log_destination_experiment_serializes_flat() {
        let dest = LogDestination::experiment("exp-123");
        let json = serde_json::to_value(&dest).unwrap();

        assert_eq!(json, json!({"experiment_id": "exp-123"}));
        // No log_id field for experiments
        assert!(json.get("log_id").is_none());
    }

    #[test]
    fn log_destination_project_logs_serializes_with_log_id() {
        let dest = LogDestination::project_logs("proj-456");
        let json = serde_json::to_value(&dest).unwrap();

        assert_eq!(json.get("project_id").unwrap(), "proj-456");
        assert_eq!(json.get("log_id").unwrap(), "g");
    }

    #[test]
    fn log_destination_playground_serializes_with_log_id() {
        let dest = LogDestination::playground_logs("session-789");
        let json = serde_json::to_value(&dest).unwrap();

        assert_eq!(json.get("prompt_session_id").unwrap(), "session-789");
        assert_eq!(json.get("log_id").unwrap(), "x");
    }

    #[test]
    fn log_destination_deserializes_experiment() {
        let json = json!({"experiment_id": "exp-123"});
        let dest: LogDestination = serde_json::from_value(json).unwrap();

        assert!(
            matches!(dest, LogDestination::Experiment { experiment_id } if experiment_id == "exp-123")
        );
    }

    #[test]
    fn log_destination_deserializes_project_logs() {
        let json = json!({"project_id": "proj-456", "log_id": "g"});
        let dest: LogDestination = serde_json::from_value(json).unwrap();

        assert!(
            matches!(dest, LogDestination::ProjectLogs { project_id, log_id }
            if project_id == "proj-456" && log_id == "g")
        );
    }

    #[test]
    fn log_destination_deserializes_playground() {
        let json = json!({"prompt_session_id": "session-789", "log_id": "x"});
        let dest: LogDestination = serde_json::from_value(json).unwrap();

        assert!(
            matches!(dest, LogDestination::PlaygroundLogs { prompt_session_id, log_id }
            if prompt_session_id == "session-789" && log_id == "x")
        );
    }

    #[test]
    fn log_destination_rejects_empty_object() {
        let json = json!({});
        let result: Result<LogDestination, _> = serde_json::from_value(json);

        assert!(result.is_err());
    }

    #[test]
    fn log_destination_rejects_missing_required_fields() {
        // project_id without log_id should fail to match ProjectLogs,
        // and won't match other variants either
        let json = json!({"project_id": "proj-456"});
        let result: Result<LogDestination, _> = serde_json::from_value(json);

        assert!(result.is_err());
    }

    #[test]
    fn logs3_row_flattens_destination() {
        let row = Logs3Row {
            id: "row-1".to_string(),
            is_merge: None,
            span_id: "span-1".to_string(),
            root_span_id: "span-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: None,
            input: None,
            output: None,
            metadata: None,
            metrics: None,
            span_attributes: None,
            created: Utc::now(),
        };

        let json = serde_json::to_value(&row).unwrap();

        // experiment_id should be at top level, not nested
        assert_eq!(json.get("experiment_id").unwrap(), "exp-123");
        assert!(json.get("destination").is_none());
        // No log_id for experiments
        assert!(json.get("log_id").is_none());
        // org_id and created are always present
        assert!(json.get("org_id").is_some());
        assert!(json.get("created").is_some());
    }

    #[test]
    fn parent_span_info_full_span_serializes_object_type_as_u8() {
        let parent = ParentSpanInfo::FullSpan {
            object_type: SpanObjectType::Experiment,
            object_id: "exp-123".to_string(),
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
        };

        let json = serde_json::to_value(&parent).unwrap();
        let obj = json.get("FullSpan").unwrap();

        // SpanObjectType serializes as u8 for wire compatibility
        assert_eq!(obj.get("object_type").unwrap(), 1);
    }

    #[test]
    fn parent_span_info_deserializes_with_typed_object_type() {
        // Deserializes from integer (wire format) into SpanObjectType
        let json = json!({
            "FullSpan": {
                "object_type": 1,
                "object_id": "exp-123",
                "span_id": "span-1",
                "root_span_id": "root-1"
            }
        });

        let parent: ParentSpanInfo = serde_json::from_value(json).unwrap();

        match parent {
            ParentSpanInfo::FullSpan { object_type, .. } => {
                assert_eq!(object_type, SpanObjectType::Experiment);
            }
            _ => panic!("Expected FullSpan variant"),
        }
    }

    #[test]
    fn parent_span_info_rejects_invalid_object_type() {
        // Invalid integer value should fail
        let json = json!({
            "FullSpan": {
                "object_type": 99,
                "object_id": "exp-123",
                "span_id": "span-1",
                "root_span_id": "root-1"
            }
        });

        let result: Result<ParentSpanInfo, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn parent_span_info_rejects_string_object_type() {
        // String value should fail (must be integer)
        let json = json!({
            "FullSpan": {
                "object_type": "Experiment",
                "object_id": "exp-123",
                "span_id": "span-1",
                "root_span_id": "root-1"
            }
        });

        let result: Result<ParentSpanInfo, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn span_object_type_try_from_u8() {
        assert_eq!(SpanObjectType::try_from(1), Ok(SpanObjectType::Experiment));
        assert_eq!(SpanObjectType::try_from(2), Ok(SpanObjectType::ProjectLogs));
        assert_eq!(
            SpanObjectType::try_from(3),
            Ok(SpanObjectType::PlaygroundLogs)
        );
        assert!(SpanObjectType::try_from(0).is_err());
        assert!(SpanObjectType::try_from(99).is_err());
    }

    #[test]
    fn span_type_serializes_as_snake_case() {
        assert_eq!(serde_json::to_value(SpanType::Llm).unwrap(), json!("llm"));
        assert_eq!(
            serde_json::to_value(SpanType::Score).unwrap(),
            json!("score")
        );
        assert_eq!(
            serde_json::to_value(SpanType::Function).unwrap(),
            json!("function")
        );
        assert_eq!(
            serde_json::to_value(SpanType::Automation).unwrap(),
            json!("automation")
        );
        assert_eq!(
            serde_json::to_value(SpanType::Facet).unwrap(),
            json!("facet")
        );
        assert_eq!(
            serde_json::to_value(SpanType::Preprocessor).unwrap(),
            json!("preprocessor")
        );
    }

    #[test]
    fn span_type_deserializes_from_snake_case() {
        let llm: SpanType = serde_json::from_value(json!("llm")).unwrap();
        assert_eq!(llm, SpanType::Llm);

        let tool: SpanType = serde_json::from_value(json!("tool")).unwrap();
        assert_eq!(tool, SpanType::Tool);

        let automation: SpanType = serde_json::from_value(json!("automation")).unwrap();
        assert_eq!(automation, SpanType::Automation);

        let facet: SpanType = serde_json::from_value(json!("facet")).unwrap();
        assert_eq!(facet, SpanType::Facet);

        let preprocessor: SpanType = serde_json::from_value(json!("preprocessor")).unwrap();
        assert_eq!(preprocessor, SpanType::Preprocessor);
    }

    #[test]
    fn span_attributes_serializes_flat_with_extras() {
        let attrs = SpanAttributes {
            name: Some("my-span".to_string()),
            span_type: Some(SpanType::Llm),
            purpose: None,
            extra: [("custom_field".to_string(), json!(42))]
                .into_iter()
                .collect(),
        };

        let json = serde_json::to_value(&attrs).unwrap();

        // All fields should be at top level (flat, not nested "extra")
        assert_eq!(json.get("name").unwrap(), "my-span");
        assert_eq!(json.get("type").unwrap(), "llm");
        assert_eq!(json.get("custom_field").unwrap(), 42);
        // No "extra" key in output
        assert!(json.get("extra").is_none());
        // purpose is None so should be omitted
        assert!(json.get("purpose").is_none());
    }

    #[test]
    fn span_attributes_deserializes_with_passthrough() {
        let json = json!({
            "name": "test-span",
            "type": "score",
            "purpose": "scorer",
            "exec_counter": 5,
            "unknown_field": "hello"
        });

        let attrs: SpanAttributes = serde_json::from_value(json).unwrap();

        assert_eq!(attrs.name, Some("test-span".to_string()));
        assert_eq!(attrs.span_type, Some(SpanType::Score));
        assert_eq!(attrs.purpose, Some("scorer".to_string()));
        // Unknown fields captured in extra
        assert_eq!(attrs.extra.get("exec_counter").unwrap(), &json!(5));
        assert_eq!(attrs.extra.get("unknown_field").unwrap(), &json!("hello"));
    }

    #[test]
    fn span_attributes_empty_serializes_to_empty_object() {
        let attrs = SpanAttributes::default();
        let json = serde_json::to_value(&attrs).unwrap();

        assert_eq!(json, json!({}));
    }
}
