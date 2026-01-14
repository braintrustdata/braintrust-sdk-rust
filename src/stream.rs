//! Stream aggregation for LLM streaming responses.
//!
//! This module provides `BraintrustStream`, a wrapper that aggregates streaming
//! chunks into a final response value, following the JS/Python SDK pattern.
//!
//! It also provides `wrap_stream_with_span` for wrapping streams with span logging.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use anyhow::Result;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::span::{SpanHandle, SpanLog, SpanSubmitter};
use crate::types::{usage_metrics_to_map, UsageMetrics};

/// A tool call in a chat message.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String, // Always "function"
    pub function: FunctionCall,
}

/// Function details in a tool call.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A chat message in the output.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ChatMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// A choice in the output array (matches OpenAI response format).
#[derive(Clone, Debug, Serialize)]
pub struct OutputChoice {
    pub index: usize,
    pub message: ChatMessage,
    pub logprobs: Option<()>, // Always None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Stream metadata with typed known fields and passthrough for extras.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StreamMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Catch-all for additional fields (passthrough behavior).
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

impl StreamMetadata {
    /// Returns true if the metadata has no content.
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.extra.is_empty()
    }

    /// Convert to a serde_json Map for logging.
    pub fn to_map(&self) -> Option<serde_json::Map<String, Value>> {
        if self.is_empty() {
            return None;
        }
        // Serialize to Value and extract the Map
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        }
    }
}

/// Aggregated result from a streaming response.
#[derive(Clone)]
pub struct FinalizedStream {
    /// The output choices (matches OpenAI response format)
    pub output: Vec<OutputChoice>,
    /// Usage metrics extracted from the stream
    pub usage: Option<UsageMetrics>,
    /// Metadata (model and any extras)
    pub metadata: StreamMetadata,
}

/// A stream aggregator that collects streaming chunks and produces a final value.
///
/// This follows the JS/Python SDK pattern where streaming responses are
/// collected and aggregated lazily when `final_value()` is called.
///
/// Raw chunks are stored as-is during streaming (non-blocking), and transformation
/// to universal format happens during aggregation (which runs async in a spawned task).
#[derive(Clone, Default)]
pub struct BraintrustStream {
    raw_chunks: Vec<Value>,
    finalized: Option<FinalizedStream>,
}

/// OpenAI-style streaming chunk structure for deserialization.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct StreamChunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<StreamUsage>,
}

/// Delta from a streaming chunk (typed for role/content).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct StreamDelta {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StreamUsage {
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default, alias = "input_tokens")]
    completion_tokens: Option<i64>,
    #[serde(default, alias = "cache_read_input_tokens")]
    prompt_cached_tokens: Option<i64>,
    #[serde(default, alias = "cache_creation_input_tokens")]
    prompt_cache_creation_tokens: Option<i64>,
}

impl BraintrustStream {
    /// Create a new empty stream.
    pub fn new() -> Self {
        Self {
            raw_chunks: Vec::new(),
            finalized: None,
        }
    }

    /// Add a raw JSON value to the stream.
    ///
    /// Stores the raw chunk as-is for later aggregation. This is non-blocking
    /// to avoid adding latency to the streaming hot path. Transformation to
    /// universal format happens lazily in `aggregate()`.
    pub fn push(&mut self, value: Value) {
        // Skip keep-alive markers
        if value.get("_keep_alive").is_some() {
            return;
        }
        self.raw_chunks.push(value);
    }

    /// Get the final aggregated value.
    ///
    /// This aggregates all chunks into a final response. The result is cached,
    /// so subsequent calls return the same value.
    pub fn final_value(&mut self) -> Result<&FinalizedStream> {
        if self.finalized.is_none() {
            self.finalized = Some(self.aggregate()?);
        }
        Ok(self.finalized.as_ref().unwrap())
    }

    /// Check if the stream has any chunks.
    pub fn is_empty(&self) -> bool {
        self.raw_chunks.is_empty()
    }

    fn aggregate(&self) -> Result<FinalizedStream> {
        let mut usage: Option<UsageMetrics> = None;
        let mut model: Option<String> = None;
        let mut finish_reason: Option<String> = None;

        // Aggregate content from all chunks
        let mut aggregated_content = String::new();
        let mut role: Option<String> = None;

        for raw in &self.raw_chunks {
            // Try to parse as OpenAI-style streaming chunk
            let chunk: StreamChunk = match serde_json::from_value(raw.clone()) {
                Ok(c) => c,
                Err(_) => continue, // Skip unparseable chunks
            };

            // Extract model (take first non-None)
            if model.is_none() {
                model = chunk.model;
            }

            // Extract usage (take last non-None)
            if let Some(ref u) = chunk.usage {
                usage = Some(UsageMetrics {
                    prompt_tokens: u.prompt_tokens.and_then(|v| u32::try_from(v).ok()),
                    completion_tokens: u.completion_tokens.and_then(|v| u32::try_from(v).ok()),
                    total_tokens: match (u.prompt_tokens, u.completion_tokens) {
                        (Some(p), Some(c)) => u32::try_from(p + c).ok(),
                        _ => None,
                    },
                    reasoning_tokens: None,
                    prompt_cached_tokens: u
                        .prompt_cached_tokens
                        .and_then(|v| u32::try_from(v).ok()),
                    prompt_cache_creation_tokens: u
                        .prompt_cache_creation_tokens
                        .and_then(|v| u32::try_from(v).ok()),
                    completion_reasoning_tokens: None,
                    prompt_tokens_details: None,
                    completion_tokens_details: None,
                });
            }

            // Process choices
            for choice in &chunk.choices {
                // Extract finish_reason (take last non-None)
                if let Some(ref reason) = choice.finish_reason {
                    if !reason.is_empty() {
                        finish_reason = Some(reason.clone());
                    }
                }

                // Extract content from delta
                if let Some(ref delta) = choice.delta {
                    // Extract role (take first)
                    if role.is_none() {
                        role = delta.role.clone();
                    }

                    // Append content
                    if let Some(ref content) = delta.content {
                        aggregated_content.push_str(content);
                    }
                }
            }
        }

        // Build metadata (finish_reason moved to OutputChoice)
        let metadata = StreamMetadata {
            model,
            extra: HashMap::new(),
        };

        // Build typed output (matches OpenAI response format)
        let message = ChatMessage {
            role: Some(role.unwrap_or_else(|| "assistant".to_string())),
            content: Some(aggregated_content),
            tool_calls: None, // TODO: implement tool call aggregation
        };

        let choice = OutputChoice {
            index: 0,
            message,
            logprobs: None,
            finish_reason,
        };

        Ok(FinalizedStream {
            output: vec![choice],
            usage,
            metadata,
        })
    }
}

/// Wrap a stream with span logging.
///
/// This creates a new stream that yields the same chunks as the original,
/// but also:
/// - Records time-to-first-token on first meaningful content
/// - Accumulates chunks for aggregation
/// - On stream completion, logs the aggregated output/usage/metadata via `span.log()`
///
/// # Type Parameters
/// - `S`: The stream type yielding `Result<Value, E>`
/// - `E`: The error type (allows use with any error type)
/// - `Sub`: The span submitter type
#[allow(private_bounds)]
pub fn wrap_stream_with_span<S, E, Sub>(
    stream: S,
    span: SpanHandle<Sub>,
) -> Pin<Box<dyn Stream<Item = std::result::Result<Value, E>> + Send>>
where
    S: Stream<Item = std::result::Result<Value, E>> + Send + Unpin + 'static,
    E: Send + 'static,
    Sub: SpanSubmitter + 'static,
{
    use futures::StreamExt;

    let start_time = Instant::now();
    let ttft_recorded = Arc::new(AtomicBool::new(false));
    let aggregator = Arc::new(Mutex::new(BraintrustStream::new()));
    let span_for_complete = span.clone();
    let aggregator_for_complete = Arc::clone(&aggregator);

    let logged_stream = stream.then(move |result| {
        let span = span.clone();
        let ttft_recorded = ttft_recorded.clone();
        let aggregator = aggregator.clone();
        async move {
            if let Ok(ref value) = result {
                // Skip keep-alive markers
                if value.get("_keep_alive").is_none() {
                    // Record TTFT on first meaningful chunk
                    if !ttft_recorded.swap(true, Ordering::SeqCst) && value_has_content(value) {
                        let ttft_secs = start_time.elapsed().as_secs_f64();
                        span.log(SpanLog {
                            metrics: Some(
                                [("time_to_first_token".to_string(), ttft_secs)]
                                    .into_iter()
                                    .collect(),
                            ),
                            ..Default::default()
                        })
                        .await;
                    }
                    // Accumulate chunk for final aggregation
                    aggregator.lock().await.push(value.clone());
                }
            }
            result
        }
    });

    // Wrap in a stream that finalizes on completion
    Box::pin(SpanCompleteWrapper {
        inner: Box::pin(logged_stream),
        span: Some(span_for_complete),
        aggregator: Some(aggregator_for_complete),
        finalize_state: FinalizeState::Idle,
    })
}

/// Check if a JSON value contains meaningful output (for TTFT detection).
fn value_has_content(value: &Value) -> bool {
    // Check for choices array with content
    if let Some(choices) = value.get("choices").and_then(|c| c.as_array()) {
        if !choices.is_empty() {
            return true;
        }
    }
    // Check for usage with tokens
    if let Some(usage) = value.get("usage").and_then(|u| u.as_object()) {
        let has_tokens = usage
            .get("completion_tokens")
            .and_then(|v| v.as_i64())
            .map(|t| t > 0)
            .unwrap_or(false)
            || usage
                .get("prompt_tokens")
                .and_then(|v| v.as_i64())
                .map(|t| t > 0)
                .unwrap_or(false);
        if has_tokens {
            return true;
        }
    }
    false
}

/// State for stream finalization.
enum FinalizeState {
    /// Not yet finalizing
    Idle,
    /// Finalizing in progress
    Finalizing(Pin<Box<dyn std::future::Future<Output = ()> + Send>>),
    /// Finalization complete
    Done,
}

/// A wrapper stream that logs aggregated output when the stream is exhausted.
struct SpanCompleteWrapper<S, Sub: SpanSubmitter> {
    inner: S,
    span: Option<SpanHandle<Sub>>,
    aggregator: Option<Arc<Mutex<BraintrustStream>>>,
    finalize_state: FinalizeState,
}

/// Finalize the stream by logging and flushing the span.
async fn finalize_span<Sub: SpanSubmitter>(
    span: SpanHandle<Sub>,
    aggregator: Arc<Mutex<BraintrustStream>>,
) {
    let mut agg = aggregator.lock().await;
    if !agg.is_empty() {
        match agg.final_value() {
            Ok(finalized) => {
                // Build metrics from usage
                let metrics = finalized
                    .usage
                    .as_ref()
                    .map(|u| usage_metrics_to_map(u.clone()));

                // Convert StreamMetadata to Option<Map>
                let metadata = finalized.metadata.to_map();

                // Serialize typed output to Value for SpanLog
                let output = serde_json::to_value(&finalized.output).ok();

                span.log(SpanLog {
                    output,
                    metadata,
                    metrics,
                    ..Default::default()
                })
                .await;
            }
            Err(e) => {
                tracing::warn!("Failed to finalize stream: {}", e);
            }
        }
    }
    // Flush span with aggregated output
    if let Err(e) = span.flush().await {
        tracing::warn!("Failed to flush span: {}", e);
    }
}

impl<S, E, Sub> Stream for SpanCompleteWrapper<S, Sub>
where
    S: Stream<Item = std::result::Result<Value, E>> + Unpin,
    E: Send + 'static,
    Sub: SpanSubmitter + 'static,
{
    type Item = std::result::Result<Value, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // SAFETY: We never move the inner stream or finalize future after pinning
        let this = unsafe { self.get_unchecked_mut() };

        // First, check if we're in the middle of finalizing
        match &mut this.finalize_state {
            FinalizeState::Idle => {
                // Not finalizing yet, poll the inner stream
            }
            FinalizeState::Finalizing(fut) => {
                // Poll the finalization future
                match fut.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        // Finalization complete
                        this.finalize_state = FinalizeState::Done;
                        return Poll::Ready(None);
                    }
                    Poll::Pending => {
                        // Still finalizing
                        return Poll::Pending;
                    }
                }
            }
            FinalizeState::Done => {
                return Poll::Ready(None);
            }
        }

        // Poll the inner stream
        let result = Pin::new(&mut this.inner).poll_next(cx);

        // If stream is done, start finalization
        if matches!(result, Poll::Ready(None)) {
            if let (Some(span), Some(aggregator)) = (this.span.take(), this.aggregator.take()) {
                // Create the finalization future
                let fut = Box::pin(finalize_span(span, aggregator));
                this.finalize_state = FinalizeState::Finalizing(fut);

                // Poll it immediately by recursing
                // SAFETY: self is still valid and pinned
                return unsafe { Pin::new_unchecked(this) }.poll_next(cx);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aggregates_content_from_streaming_values() {
        let chunks = vec![
            json!({
                "id": "chunk1",
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "content": "Hello" }
                }],
                "created": 1
            }),
            json!({
                "id": "chunk2",
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "delta": { "content": " world" }
                }],
                "created": 1
            }),
            json!({
                "id": "chunk3",
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "!" },
                    "finish_reason": "stop"
                }],
                "created": 1
            }),
        ];

        let mut stream = BraintrustStream::new();
        for chunk in chunks {
            stream.push(chunk);
        }

        let finalized = stream.final_value().expect("should finalize");

        // Check output is array of choices
        assert_eq!(finalized.output.len(), 1);

        let choice = &finalized.output[0];
        assert_eq!(choice.index, 0);
        assert_eq!(choice.message.role.as_deref(), Some("assistant"));
        assert_eq!(choice.message.content.as_deref(), Some("Hello world!"));
        assert_eq!(choice.finish_reason.as_deref(), Some("stop"));

        // Check metadata
        assert_eq!(finalized.metadata.model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn aggregates_usage_from_final_chunk() {
        let chunks = vec![
            json!({
                "id": "chunk1",
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "content": "Hi" },
                    "finish_reason": "stop"
                }],
                "created": 1
            }),
            json!({
                "id": "chunk2",
                "model": "gpt-4",
                "choices": [],
                "created": 1,
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5
                }
            }),
        ];

        let mut stream = BraintrustStream::new();
        for chunk in chunks {
            stream.push(chunk);
        }

        let finalized = stream.final_value().expect("should finalize");

        let usage = finalized.usage.as_ref().expect("should have usage");
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(15));
    }

    #[test]
    fn skips_keep_alive_markers() {
        let mut stream = BraintrustStream::new();

        // Push a keep-alive marker
        stream.push(json!({"_keep_alive": true}));

        assert!(stream.is_empty());
    }

    #[test]
    fn caches_finalized_result() {
        let chunk = json!({
            "id": "chunk1",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "test" }
            }],
            "created": 1
        });

        let mut stream = BraintrustStream::new();
        stream.push(chunk);

        // First call computes - extract content and drop borrow
        let first_content = {
            let first = stream.final_value().expect("should finalize");
            first.output.first().and_then(|c| c.message.content.clone())
        };

        // Second call returns cached
        let second_content = {
            let second = stream.final_value().expect("should finalize");
            second
                .output
                .first()
                .and_then(|c| c.message.content.clone())
        };

        assert_eq!(first_content, second_content);
        assert_eq!(first_content, Some("test".to_string()));
    }
}
