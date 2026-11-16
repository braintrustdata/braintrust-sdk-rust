use crate::UsageMetrics;
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashMap, fmt};

// =============================================================================
// Error types for stream output builders (empty for now, extensible later)
// =============================================================================

/// Error type for ToolCall builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ToolCallBuilderError {}

impl fmt::Display for ToolCallBuilderError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for ToolCallBuilderError {}

/// Error type for FunctionCall builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FunctionCallBuilderError {}

impl fmt::Display for FunctionCallBuilderError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for FunctionCallBuilderError {}

/// Error type for ChatMessage builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ChatMessageBuilderError {}

impl fmt::Display for ChatMessageBuilderError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for ChatMessageBuilderError {}

/// Error type for OutputChoice builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum OutputChoiceBuilderError {}

impl fmt::Display for OutputChoiceBuilderError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for OutputChoiceBuilderError {}

/// Error type for StreamMetadata builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StreamMetadataBuilderError {}

impl fmt::Display for StreamMetadataBuilderError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for StreamMetadataBuilderError {}

/// Error type for FinalizedStream builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FinalizedStreamBuilderError {}

impl fmt::Display for FinalizedStreamBuilderError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for FinalizedStreamBuilderError {}

/// A tool call in a chat message.
#[derive(Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String, // Always "function"
    function: FunctionCall,
}

impl ToolCall {
    /// Create a new ToolCallBuilder.
    pub fn builder() -> ToolCallBuilder {
        ToolCallBuilder::new()
    }

    /// Create a new ToolCall (internal use).
    #[allow(dead_code)]
    pub(crate) fn new(id: String, call_type: String, function: FunctionCall) -> Self {
        Self {
            id,
            call_type,
            function,
        }
    }

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

/// Builder for ToolCall.
#[derive(Clone, Default)]
pub struct ToolCallBuilder {
    id: String,
    call_type: String,
    function: FunctionCall,
}

impl ToolCallBuilder {
    /// Create a new ToolCallBuilder.
    pub fn new() -> Self {
        Self {
            call_type: "function".to_string(),
            ..Default::default()
        }
    }

    /// Set the tool call ID.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Set the call type.
    pub fn call_type(mut self, call_type: impl Into<String>) -> Self {
        self.call_type = call_type.into();
        self
    }

    /// Set the function details.
    pub fn function(mut self, function: FunctionCall) -> Self {
        self.function = function;
        self
    }

    /// Build the ToolCall.
    pub fn build(self) -> std::result::Result<ToolCall, ToolCallBuilderError> {
        Ok(ToolCall {
            id: self.id,
            call_type: self.call_type,
            function: self.function,
        })
    }
}

/// Function details in a tool call.
#[derive(Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct FunctionCall {
    name: String,
    arguments: String,
}

impl FunctionCall {
    /// Create a new FunctionCallBuilder.
    pub fn builder() -> FunctionCallBuilder {
        FunctionCallBuilder::new()
    }

    /// Create a new FunctionCall (internal use).
    #[allow(dead_code)]
    pub(crate) fn new(name: String, arguments: String) -> Self {
        Self { name, arguments }
    }

    /// Get the function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the function arguments (typically JSON string).
    pub fn arguments(&self) -> &str {
        &self.arguments
    }
}

/// Builder for FunctionCall.
#[derive(Clone, Default)]
pub struct FunctionCallBuilder {
    name: String,
    arguments: String,
}

impl FunctionCallBuilder {
    /// Create a new FunctionCallBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the function name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the function arguments.
    pub fn arguments(mut self, arguments: impl Into<String>) -> Self {
        self.arguments = arguments.into();
        self
    }

    /// Build the FunctionCall.
    pub fn build(self) -> std::result::Result<FunctionCall, FunctionCallBuilderError> {
        Ok(FunctionCall {
            name: self.name,
            arguments: self.arguments,
        })
    }
}

/// A chat message in the output.
#[derive(Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct ChatMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    /// Create a new ChatMessageBuilder.
    pub fn builder() -> ChatMessageBuilder {
        ChatMessageBuilder::new()
    }

    /// Create a new ChatMessage (internal use).
    pub(crate) fn new(
        role: Option<String>,
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    ) -> Self {
        Self {
            role,
            content,
            tool_calls,
        }
    }

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

/// Builder for ChatMessage.
#[derive(Clone, Default)]
pub struct ChatMessageBuilder {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessageBuilder {
    /// Create a new ChatMessageBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the message role.
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    /// Set the message content.
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = Some(content.into());
        self
    }

    /// Set the tool calls.
    pub fn tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = Some(tool_calls);
        self
    }

    /// Build the ChatMessage.
    pub fn build(self) -> std::result::Result<ChatMessage, ChatMessageBuilderError> {
        Ok(ChatMessage {
            role: self.role,
            content: self.content,
            tool_calls: self.tool_calls,
        })
    }
}

/// A choice in the output array (matches OpenAI response format).
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct OutputChoice {
    index: usize,
    message: ChatMessage,
    logprobs: Option<()>, // Always None
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

impl OutputChoice {
    /// Create a new OutputChoiceBuilder.
    pub fn builder() -> OutputChoiceBuilder {
        OutputChoiceBuilder::new()
    }

    /// Create a new OutputChoice (internal use).
    pub(crate) fn new(index: usize, message: ChatMessage, finish_reason: Option<String>) -> Self {
        Self {
            index,
            message,
            logprobs: None,
            finish_reason,
        }
    }

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

/// Builder for OutputChoice.
#[derive(Clone, Default)]
pub struct OutputChoiceBuilder {
    index: usize,
    message: ChatMessage,
    finish_reason: Option<String>,
}

impl OutputChoiceBuilder {
    /// Create a new OutputChoiceBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the choice index.
    pub fn index(mut self, index: usize) -> Self {
        self.index = index;
        self
    }

    /// Set the message.
    pub fn message(mut self, message: ChatMessage) -> Self {
        self.message = message;
        self
    }

    /// Set the finish reason.
    pub fn finish_reason(mut self, finish_reason: impl Into<String>) -> Self {
        self.finish_reason = Some(finish_reason.into());
        self
    }

    /// Build the OutputChoice.
    pub fn build(self) -> std::result::Result<OutputChoice, OutputChoiceBuilderError> {
        Ok(OutputChoice {
            index: self.index,
            message: self.message,
            logprobs: None,
            finish_reason: self.finish_reason,
        })
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

impl StreamMetadata {
    /// Create a new StreamMetadataBuilder.
    pub fn builder() -> StreamMetadataBuilder {
        StreamMetadataBuilder::new()
    }

    /// Create a new StreamMetadata (internal use).
    pub(crate) fn new(model: Option<String>, extra: HashMap<String, Value>) -> Self {
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

/// Builder for StreamMetadata.
#[derive(Clone, Default)]
pub struct StreamMetadataBuilder {
    model: Option<String>,
    extra: HashMap<String, Value>,
}

impl StreamMetadataBuilder {
    /// Create a new StreamMetadataBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the model name.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Add an extra field.
    pub fn extra_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extra.insert(key.into(), value.into());
        self
    }

    /// Set all extra fields.
    pub fn extra(mut self, extra: HashMap<String, Value>) -> Self {
        self.extra = extra;
        self
    }

    /// Build the StreamMetadata.
    pub fn build(self) -> std::result::Result<StreamMetadata, StreamMetadataBuilderError> {
        Ok(StreamMetadata {
            model: self.model,
            extra: self.extra,
        })
    }
}

/// Aggregated result from a streaming response.
#[derive(Clone)]
#[non_exhaustive]
pub struct FinalizedStream {
    /// The output choices (matches OpenAI response format)
    output: Vec<OutputChoice>,
    /// Usage metrics extracted from the stream
    usage: Option<UsageMetrics>,
    /// Metadata (model and any extras)
    metadata: StreamMetadata,
}

impl FinalizedStream {
    /// Create a new FinalizedStreamBuilder.
    pub fn builder() -> FinalizedStreamBuilder {
        FinalizedStreamBuilder::new()
    }

    /// Create a new FinalizedStream (internal use).
    pub(crate) fn new(
        output: Vec<OutputChoice>,
        usage: Option<UsageMetrics>,
        metadata: StreamMetadata,
    ) -> Self {
        Self {
            output,
            usage,
            metadata,
        }
    }

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

/// Builder for FinalizedStream.
#[derive(Clone, Default)]
pub struct FinalizedStreamBuilder {
    output: Vec<OutputChoice>,
    usage: Option<UsageMetrics>,
    metadata: StreamMetadata,
}

impl FinalizedStreamBuilder {
    /// Create a new FinalizedStreamBuilder.
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Set the usage metrics.
    pub fn usage(mut self, usage: UsageMetrics) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Set the metadata.
    pub fn metadata(mut self, metadata: StreamMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Build the FinalizedStream.
    pub fn build(self) -> std::result::Result<FinalizedStream, FinalizedStreamBuilderError> {
        Ok(FinalizedStream {
            output: self.output,
            usage: self.usage,
            metadata: self.metadata,
        })
    }
}
