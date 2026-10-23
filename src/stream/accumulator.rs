//! Provider-independent retained output state; each parsed chunk is applied once.
use super::update::Update;
use super::{ChatMessage, FinalizedStream, OutputChoice, StreamMetadata};
use crate::UsageMetrics;
use std::collections::{BTreeMap, HashMap};

/// Accumulates partial tool call information from streaming chunks.
///
/// In OpenAI streaming, each chunk may contain partial tool call data.
/// This struct accumulates the id, type, function name, and raw arguments
/// across multiple chunks, then parses the complete arguments string
/// as JSON when aggregation is complete.
#[derive(Clone, Debug, Default)]
struct StreamToolCallAccumulator {
    id: String,
    call_type: String,
    function_name: String,
    raw_arguments: String,
}

impl StreamToolCallAccumulator {
    /// Creates a `ToolCall` from the accumulated data.
    ///
    /// The raw arguments string is parsed as JSON to extract the function
    /// name and arguments, falling back to the raw string if parsing fails.
    pub fn build(self) -> super::ToolCall {
        let args: serde_json::Value = serde_json::from_str(&self.raw_arguments).unwrap_or_default();

        // Extract function name from JSON object, falling back to stored name
        let function_name = if let Some(obj) = args.as_object() {
            obj.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.function_name)
                .to_string()
        } else {
            self.function_name.clone()
        };

        // Extract arguments string from JSON object, falling back to raw string
        let arguments = if let Some(obj) = args.as_object() {
            obj.get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.raw_arguments)
                .to_string()
        } else {
            self.raw_arguments.clone()
        };

        let function = super::FunctionCall::new(function_name, arguments);
        super::ToolCall::new(self.id, self.call_type, function)
    }
}

#[derive(Clone, Default)]
pub(super) struct Accumulator {
    model: Option<String>,
    usage: Option<UsageMetrics>,
    finish_reason: Option<String>,
    role: Option<String>,
    aggregated_content: String,
    tool_calls_by_index: BTreeMap<usize, StreamToolCallAccumulator>,
}
impl Accumulator {
    pub(super) fn apply(&mut self, chunk: Update) {
        // Extract model (take first non-None)
        if self.model.is_none() {
            self.model = chunk.model;
        }

        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }

        // Process choices
        for choice in &chunk.choices {
            // Extract finish_reason (take last non-None)
            if let Some(ref reason) = choice.finish_reason {
                if !reason.is_empty() {
                    self.finish_reason = Some(reason.clone());
                }
            }

            // Extract content and tool calls from delta
            if let Some(ref delta) = choice.delta {
                // Extract role (take first)
                if self.role.is_none() {
                    self.role = delta.role.clone();
                }

                // Append content
                if let Some(ref content) = delta.content {
                    self.aggregated_content.push_str(content);
                }

                // Accumulate tool calls by index
                if let Some(ref tool_calls) = delta.tool_calls {
                    for tc in tool_calls {
                        let idx = tc.index.unwrap_or(0);
                        let entry = self.tool_calls_by_index.entry(idx).or_default();

                        // Store id if present and not yet stored
                        if entry.id.is_empty() {
                            entry.id = tc.id.clone().unwrap_or_default();
                        }

                        // Store call_type if present and not yet stored
                        if entry.call_type.is_empty() {
                            entry.call_type = tc.call_type.clone().unwrap_or_default();
                        }

                        // Store function name if present
                        if let Some(ref func) = tc.function {
                            if let Some(ref name) = func.name {
                                entry.function_name = name.clone();
                            }
                            // Accumulate raw argument fragments
                            if let Some(ref args) = func.arguments {
                                if !args.is_empty() {
                                    entry.raw_arguments.push_str(args);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    pub(super) fn snapshot(&self) -> FinalizedStream {
        // Build metadata (finish_reason moved to OutputChoice)
        let metadata = StreamMetadata::new(self.model.clone(), HashMap::new());

        // Build typed output (matches OpenAI response format)
        let message = if self.tool_calls_by_index.is_empty() {
            ChatMessage::new(
                Some(self.role.clone().unwrap_or_else(|| "assistant".to_string())),
                Some(self.aggregated_content.clone()),
                None,
            )
        } else {
            let tool_calls = self
                .tool_calls_by_index
                .values()
                .cloned()
                .map(StreamToolCallAccumulator::build)
                .collect();
            ChatMessage::new(
                Some(self.role.clone().unwrap_or_else(|| "assistant".to_string())),
                Some(self.aggregated_content.clone()),
                Some(tool_calls),
            )
        };

        let choice = OutputChoice::new(0, message, self.finish_reason.clone());

        FinalizedStream::new(vec![choice], self.usage.clone(), metadata)
    }
}
