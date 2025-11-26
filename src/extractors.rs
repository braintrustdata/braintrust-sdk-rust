use crate::types::{CompletionTokensDetails, PromptTokensDetails, UsageMetrics};
use serde_json::Value;

pub fn extract_openai_usage(value: &Value) -> UsageMetrics {
    if let Some(usage) = value.get("usage").and_then(Value::as_object) {
        let prompt_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let completion_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let total_tokens = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or_else(|| match (prompt_tokens, completion_tokens) {
                (Some(prompt), Some(completion)) => Some(prompt + completion),
                _ => None,
            });

        let prompt_details =
            parse_prompt_tokens_details(usage, &["prompt_tokens_details", "input_tokens_details"]);
        let completion_details = parse_completion_tokens_details(
            usage,
            &["completion_tokens_details", "output_tokens_details"],
        );

        let prompt_cached_tokens = usage
            .get("prompt_cached_tokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or_else(|| {
                prompt_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens)
            });
        let prompt_cache_creation_tokens = usage
            .get("prompt_cache_creation_tokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or_else(|| {
                prompt_details
                    .as_ref()
                    .and_then(|details| details.cache_creation_tokens)
            });
        let reasoning_tokens = usage
            .get("reasoning_tokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or_else(|| {
                completion_details
                    .as_ref()
                    .and_then(|details| details.reasoning_tokens)
            });

        UsageMetrics {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            reasoning_tokens,
            prompt_cached_tokens,
            prompt_cache_creation_tokens,
            completion_reasoning_tokens: reasoning_tokens,
            prompt_tokens_details: prompt_details,
            completion_tokens_details: completion_details,
        }
    } else {
        UsageMetrics::default()
    }
}

pub fn extract_anthropic_usage(value: &Value) -> UsageMetrics {
    if let Some(usage) = value.get("usage").and_then(Value::as_object) {
        let prompt = usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"));
        let completion = usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"));

        let prompt_tokens = prompt.and_then(Value::as_u64).map(|v| v as u32);
        let completion_tokens = completion.and_then(Value::as_u64).map(|v| v as u32);

        let total_tokens = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .or_else(|| match (prompt_tokens, completion_tokens) {
                (Some(p), Some(c)) => Some(p + c),
                _ => None,
            });

        let prompt_cached_tokens = usage
            .get("cache_read_input_tokens")
            .or_else(|| usage.get("prompt_cache_read_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let prompt_cache_creation_tokens = usage
            .get("cache_creation_input_tokens")
            .or_else(|| usage.get("prompt_cache_creation_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32);

        let prompt_tokens_details =
            if prompt_cached_tokens.is_some() || prompt_cache_creation_tokens.is_some() {
                Some(PromptTokensDetails {
                    cached_tokens: prompt_cached_tokens,
                    cache_creation_tokens: prompt_cache_creation_tokens,
                    audio_tokens: None,
                })
            } else {
                None
            };

        UsageMetrics {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            reasoning_tokens: usage
                .get("reasoning_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            prompt_cached_tokens,
            prompt_cache_creation_tokens,
            completion_reasoning_tokens: usage
                .get("reasoning_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            prompt_tokens_details,
            completion_tokens_details: usage
                .get("output_tokens_details")
                .and_then(Value::as_object)
                .map(|details| CompletionTokensDetails {
                    reasoning_tokens: details
                        .get("reasoning_tokens")
                        .and_then(Value::as_u64)
                        .map(|v| v as u32),
                    audio_tokens: details
                        .get("audio_tokens")
                        .and_then(Value::as_u64)
                        .map(|v| v as u32),
                    accepted_prediction_tokens: None,
                    rejected_prediction_tokens: None,
                }),
        }
    } else {
        UsageMetrics::default()
    }
}

fn parse_prompt_tokens_details(
    usage: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<PromptTokensDetails> {
    keys.iter()
        .find_map(|key| usage.get(*key))
        .and_then(Value::as_object)
        .map(|details| PromptTokensDetails {
            audio_tokens: details
                .get("audio_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            cached_tokens: details
                .get("cached_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            cache_creation_tokens: details
                .get("cache_creation_tokens")
                .or_else(|| details.get("cache_creation_input_tokens"))
                .and_then(Value::as_u64)
                .map(|v| v as u32),
        })
}

fn parse_completion_tokens_details(
    usage: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<CompletionTokensDetails> {
    keys.iter()
        .find_map(|key| usage.get(*key))
        .and_then(Value::as_object)
        .map(|details| CompletionTokensDetails {
            reasoning_tokens: details
                .get("reasoning_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            accepted_prediction_tokens: details
                .get("accepted_prediction_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            rejected_prediction_tokens: details
                .get("rejected_prediction_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            audio_tokens: details
                .get("audio_tokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_openai_usage_details() {
        let value = json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 200,
                "total_tokens": 300,
                "prompt_tokens_details": {
                    "cached_tokens": 80,
                    "cache_creation_tokens": 10,
                    "audio_tokens": 5
                },
                "completion_tokens_details": {
                    "reasoning_tokens": 40,
                    "accepted_prediction_tokens": 7,
                    "rejected_prediction_tokens": 3,
                    "audio_tokens": 2
                }
            }
        });

        let metrics = extract_openai_usage(&value);
        assert_eq!(metrics.prompt_tokens, Some(100));
        assert_eq!(metrics.completion_tokens, Some(200));
        assert_eq!(metrics.total_tokens, Some(300));
        assert_eq!(metrics.prompt_cached_tokens, Some(80));
        assert_eq!(metrics.prompt_cache_creation_tokens, Some(10));
        assert_eq!(metrics.completion_reasoning_tokens, Some(40));
        let prompt_details = metrics.prompt_tokens_details.expect("prompt details");
        assert_eq!(prompt_details.audio_tokens, Some(5));
        assert_eq!(prompt_details.cached_tokens, Some(80));
        assert_eq!(prompt_details.cache_creation_tokens, Some(10));
        let completion_details = metrics
            .completion_tokens_details
            .expect("completion details");
        assert_eq!(completion_details.reasoning_tokens, Some(40));
        assert_eq!(completion_details.accepted_prediction_tokens, Some(7));
        assert_eq!(completion_details.rejected_prediction_tokens, Some(3));
        assert_eq!(completion_details.audio_tokens, Some(2));
    }

    #[test]
    fn extracts_anthropic_cache_metrics() {
        let value = json!({
            "usage": {
                "input_tokens": 50,
                "output_tokens": 25,
                "cache_read_input_tokens": 30,
                "cache_creation_input_tokens": 5,
                "reasoning_tokens": 12
            }
        });

        let metrics = extract_anthropic_usage(&value);
        assert_eq!(metrics.prompt_tokens, Some(50));
        assert_eq!(metrics.completion_tokens, Some(25));
        assert_eq!(metrics.total_tokens, Some(75));
        assert_eq!(metrics.prompt_cached_tokens, Some(30));
        assert_eq!(metrics.prompt_cache_creation_tokens, Some(5));
        assert_eq!(metrics.completion_reasoning_tokens, Some(12));
        let prompt_details = metrics.prompt_tokens_details.expect("prompt details");
        assert_eq!(prompt_details.cached_tokens, Some(30));
        assert_eq!(prompt_details.cache_creation_tokens, Some(5));
    }
}
