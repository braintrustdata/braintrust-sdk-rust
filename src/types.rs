use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

pub const LOGS_API_VERSION: u8 = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct Logs3Request {
    pub rows: Vec<Logs3Row>,
    pub api_version: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct Logs3Row {
    pub id: String,
    pub span_id: String,
    pub root_span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_parents: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
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
    pub span_attributes: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct SpanPayload {
    pub org_id: String,
    pub org_name: Option<String>,
    pub project_name: Option<String>,
    pub name: Option<String>,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub metadata: Option<Map<String, Value>>,
    pub metrics: Option<HashMap<String, f64>>,
    pub span_attributes: Option<Map<String, Value>>,
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
        object_type: u8,
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
