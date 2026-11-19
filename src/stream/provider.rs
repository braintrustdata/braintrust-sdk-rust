//! Parsing of the existing OpenAI-compatible stream format.
use serde::Deserialize;
use serde_json::Value;

/// OpenAI-style streaming chunk structure for deserialization.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamChunk {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<StreamUsage>,
}

/// Delta from a streaming chunk (typed for role/content/tool_calls).
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct StreamDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

/// A partial tool call from a streaming delta.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct StreamToolCall {
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    #[serde(default)]
    pub function: Option<StreamFunctionCall>,
}

/// A partial function call from a streaming delta.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct StreamFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamChoice {
    #[serde(default)]
    pub delta: Option<StreamDelta>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct StreamUsage {
    #[serde(default, alias = "input_tokens")]
    prompt_tokens: Option<i64>,
    #[serde(default, alias = "output_tokens")]
    completion_tokens: Option<i64>,
    #[serde(default, alias = "completion_reasoning_tokens")]
    completion_reasoning_tokens: Option<i64>,
    #[serde(default, alias = "accepted_prediction_tokens")]
    accepted_prediction_tokens: Option<i64>,
    #[serde(default, alias = "rejected_prediction_tokens")]
    rejected_prediction_tokens: Option<i64>,
    #[serde(
        default,
        alias = "prompt_tokens_details",
        alias = "input_tokens_details"
    )]
    prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(
        default,
        alias = "completion_tokens_details",
        alias = "output_tokens_details"
    )]
    completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PromptTokensDetails {
    #[serde(default, alias = "audio_tokens", alias = "prompt_audio_tokens")]
    audio_tokens: Option<i64>,
    #[serde(default, alias = "cached_tokens", alias = "prompt_cached_tokens")]
    cached_tokens: Option<i64>,
    #[serde(
        default,
        alias = "cache_creation_tokens",
        alias = "cache_creation_input_tokens",
        alias = "prompt_cache_creation_tokens"
    )]
    cache_creation_tokens: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CompletionTokensDetails {
    #[serde(default, alias = "audio_tokens", alias = "completion_audio_tokens")]
    audio_tokens: Option<i64>,
    #[serde(
        default,
        alias = "reasoning_tokens",
        alias = "completion_reasoning_tokens"
    )]
    reasoning_tokens: Option<i64>,
}

/// Meaningful output, excluding role-only, empty-choice and usage-only chunks.
pub(super) fn has_content(value: &Value) -> bool {
    value
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                let Some(delta) = choice.get("delta") else {
                    return false;
                };
                delta
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                    || delta
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .is_some_and(|calls| !calls.is_empty())
            })
        })
}

pub(super) fn normalize_usage(u: &StreamUsage) -> crate::UsageMetrics {
    let prompt_tokens = u.prompt_tokens.and_then(|v| u32::try_from(v).ok());
    let completion_tokens = u.completion_tokens.and_then(|v| u32::try_from(v).ok());

    let prompt_details = u.prompt_tokens_details.clone().map(|p| {
        crate::types::PromptTokensDetails::new(
            p.audio_tokens.and_then(|v| u32::try_from(v).ok()),
            p.cached_tokens.and_then(|v| u32::try_from(v).ok()),
            p.cache_creation_tokens.and_then(|v| u32::try_from(v).ok()),
        )
    });

    let completion_details = u.completion_tokens_details.clone().map(|c| {
        crate::types::CompletionTokensDetails::new(
            c.audio_tokens.and_then(|v| u32::try_from(v).ok()),
            c.reasoning_tokens.and_then(|v| u32::try_from(v).ok()),
            u.accepted_prediction_tokens
                .and_then(|v| u32::try_from(v).ok()),
            u.rejected_prediction_tokens
                .and_then(|v| u32::try_from(v).ok()),
        )
    });

    crate::UsageMetrics {
        prompt_tokens,
        completion_tokens,
        total_tokens: match (prompt_tokens, completion_tokens) {
            (Some(p), Some(c)) => p.checked_add(c),
            _ => None,
        },
        reasoning_tokens: None,
        prompt_cached_tokens: prompt_details
            .as_ref()
            .and_then(|d| d.cached_tokens())
            .or_else(|| {
                u.prompt_tokens_details
                    .as_ref()
                    .and_then(|p| p.cached_tokens)
                    .and_then(|v| u32::try_from(v).ok())
            }),
        prompt_cache_creation_tokens: prompt_details
            .as_ref()
            .and_then(|d| d.cache_creation_tokens())
            .or_else(|| {
                u.prompt_tokens_details
                    .as_ref()
                    .and_then(|p| p.cache_creation_tokens)
                    .and_then(|v| u32::try_from(v).ok())
            }),
        completion_reasoning_tokens: u
            .completion_reasoning_tokens
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| {
                completion_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens())
            }),
        prompt_tokens_details: prompt_details,
        completion_tokens_details: completion_details,
    }
}
