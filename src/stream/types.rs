use crate::UsageMetrics;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// A tool call in a chat message.
#[derive(bon::Builder, Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct ToolCall {
    #[builder(default, into)]
    id: String,
    #[serde(rename = "type")]
    #[builder(default = "function".to_string(), into)]
    call_type: String, // Always "function"
    #[builder(default)]
    function: FunctionCall,
}

impl ToolCall {
    /// Get the tool call ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the tool call type (typically "function").
    pub fn call_type(&self) -> &str {
        &self.call_type
    }

    /// Get the function details.
    pub fn function(&self) -> &FunctionCall {
        &self.function
    }
}

/// Function details in a tool call.
#[derive(bon::Builder, Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct FunctionCall {
    #[builder(default, into)]
    name: String,
    #[builder(default, into)]
    arguments: String,
}

impl FunctionCall {
    /// Get the function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the function arguments (typically JSON string).
    pub fn arguments(&self) -> &str {
        &self.arguments
    }
}

/// A chat message in the output.
#[derive(bon::Builder, Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct ChatMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    /// Get the message role.
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    /// Get the message content.
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    /// Get the tool calls.
    pub fn tool_calls(&self) -> Option<&[ToolCall]> {
        self.tool_calls.as_deref()
    }
}

/// A choice in the output array (matches OpenAI response format).
#[derive(bon::Builder, Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct OutputChoice {
    #[builder(default)]
    index: usize,
    #[builder(default)]
    message: ChatMessage,
    #[builder(skip)]
    logprobs: Option<()>, // Always None
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    finish_reason: Option<String>,
}

impl OutputChoice {
    /// Get the choice index.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Get the message.
    pub fn message(&self) -> &ChatMessage {
        &self.message
    }

    /// Get the finish reason.
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }
}

/// Stream metadata with typed known fields and passthrough for extras.
#[derive(Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct StreamMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Catch-all for additional fields (passthrough behavior).
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    extra: HashMap<String, Value>,
}

#[bon::bon]
impl StreamMetadata {
    /// Build stream metadata with optional model and extra fields.
    #[builder]
    pub fn new(
        #[builder(field)] extra: HashMap<String, Value>,
        #[builder(into)] model: Option<String>,
    ) -> Self {
        Self { model, extra }
    }

    /// Get the model name.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Get the extra metadata fields.
    pub fn extra(&self) -> &HashMap<String, Value> {
        &self.extra
    }

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
#[derive(bon::Builder, Clone)]
#[non_exhaustive]
pub struct FinalizedStream {
    /// The output choices (matches OpenAI response format)
    #[builder(field)]
    output: Vec<OutputChoice>,
    /// Usage metrics extracted from the stream
    usage: Option<UsageMetrics>,
    /// Metadata (model and any extras)
    #[builder(default)]
    metadata: StreamMetadata,
}

impl FinalizedStream {
    /// Get the output choices.
    pub fn output(&self) -> &[OutputChoice] {
        &self.output
    }

    /// Get the usage metrics.
    pub fn usage(&self) -> Option<&UsageMetrics> {
        self.usage.as_ref()
    }

    /// Get the metadata.
    pub fn metadata(&self) -> &StreamMetadata {
        &self.metadata
    }
}

impl<S: stream_metadata_builder::State> StreamMetadataBuilder<S> {
    /// Add an extra metadata field.
    pub fn extra_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extra.insert(key.into(), value.into());
        self
    }

    /// Set all extra metadata fields.
    pub fn extra(mut self, extra: HashMap<String, Value>) -> Self {
        self.extra = extra;
        self
    }
}

impl<S: finalized_stream_builder::State> FinalizedStreamBuilder<S> {
    /// Set the output choices.
    pub fn output(mut self, output: Vec<OutputChoice>) -> Self {
        self.output = output;
        self
    }

    /// Add an output choice.
    pub fn add_output(mut self, choice: OutputChoice) -> Self {
        self.output.push(choice);
        self
    }
}
