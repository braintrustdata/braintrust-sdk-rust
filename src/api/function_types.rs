//! Shared function and prompt schema types.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Braintrust function kind.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FunctionType {
    Llm,
    Scorer,
    Task,
    Tool,
}

/// Template engine used by prompt data.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TemplateFormat {
    Mustache,
    Nunjucks,
    None,
}

/// Function invocation streaming mode.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamingMode {
    Auto,
    Parallel,
    Json,
    Text,
}

/// Reference to a saved Braintrust function.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SavedFunctionId {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_type: Option<FunctionType>,
    // TODO: Harden this Value-backed extension map to explicit function reference variants.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Chat message payload used by prompt and invocation APIs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChatCompletionMessage {
    System(ChatCompletionSystemMessage),
    User(ChatCompletionUserMessage),
    Assistant(ChatCompletionAssistantMessage),
    Function(ChatCompletionFunctionMessage),
    Tool(ChatCompletionToolMessage),
    Developer(ChatCompletionDeveloperMessage),
    Model(ChatCompletionFallbackMessage),
}

impl Default for ChatCompletionMessage {
    fn default() -> Self {
        Self::Assistant(ChatCompletionAssistantMessage::default())
    }
}

impl ChatCompletionMessage {
    pub fn system(content: impl Into<ChatCompletionTextContent>) -> Self {
        Self::System(ChatCompletionSystemMessage::new(content))
    }

    pub fn developer(content: impl Into<ChatCompletionTextContent>) -> Self {
        Self::Developer(ChatCompletionDeveloperMessage::new(content))
    }

    pub fn user(content: impl Into<ChatCompletionContent>) -> Self {
        Self::User(ChatCompletionUserMessage::new(content))
    }

    pub fn assistant(content: impl Into<ChatCompletionAssistantContent>) -> Self {
        Self::Assistant(ChatCompletionAssistantMessage::new(Some(content.into())))
    }

    pub fn assistant_empty() -> Self {
        Self::Assistant(ChatCompletionAssistantMessage::new(None))
    }

    pub fn tool(
        content: impl Into<ChatCompletionTextContent>,
        tool_call_id: impl Into<String>,
    ) -> Self {
        Self::Tool(ChatCompletionToolMessage::new(content, tool_call_id))
    }

    pub fn function(name: impl Into<String>, content: Option<String>) -> Self {
        Self::Function(ChatCompletionFunctionMessage::new(name, content))
    }

    pub fn model(content: Option<ChatCompletionNullableString>) -> Self {
        Self::Model(ChatCompletionFallbackMessage::new(content))
    }
}

/// OpenAI-compatible chat message role.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Function,
    Tool,
    Model,
    Developer,
}

/// String content or an array of text-only content parts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ChatCompletionTextContent {
    Text(String),
    Parts(Vec<ChatCompletionContentPartText>),
}

impl From<String> for ChatCompletionTextContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for ChatCompletionTextContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<ChatCompletionContentPartText>> for ChatCompletionTextContent {
    fn from(value: Vec<ChatCompletionContentPartText>) -> Self {
        Self::Parts(value)
    }
}

/// User message content: text or an array of mixed content parts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ChatCompletionContent {
    Text(String),
    Parts(Vec<ChatCompletionContentPart>),
}

impl From<String> for ChatCompletionContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for ChatCompletionContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<ChatCompletionContentPart>> for ChatCompletionContent {
    fn from(value: Vec<ChatCompletionContentPart>) -> Self {
        Self::Parts(value)
    }
}

/// Assistant message content: text, text-only parts, or explicit null.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ChatCompletionAssistantContent {
    Text(String),
    Parts(Vec<ChatCompletionContentPartText>),
    Null,
}

impl From<String> for ChatCompletionAssistantContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for ChatCompletionAssistantContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<ChatCompletionContentPartText>> for ChatCompletionAssistantContent {
    fn from(value: Vec<ChatCompletionContentPartText>) -> Self {
        Self::Parts(value)
    }
}

/// Nullable string wrapper for role variants whose content may be null.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ChatCompletionNullableString {
    Text(String),
    Null,
}

impl From<String> for ChatCompletionNullableString {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for ChatCompletionNullableString {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

/// Mixed content part accepted by user messages.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ChatCompletionContentPart {
    Text(ChatCompletionContentPartText),
    Image(ChatCompletionContentPartImage),
    File(ChatCompletionContentPartFile),
}

impl From<ChatCompletionContentPartText> for ChatCompletionContentPart {
    fn from(value: ChatCompletionContentPartText) -> Self {
        Self::Text(value)
    }
}

impl From<ChatCompletionContentPartImage> for ChatCompletionContentPart {
    fn from(value: ChatCompletionContentPartImage) -> Self {
        Self::Image(value)
    }
}

impl From<ChatCompletionContentPartFile> for ChatCompletionContentPart {
    fn from(value: ChatCompletionContentPartFile) -> Self {
        Self::File(value)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ChatCompletionContentPartTextType {
    Text,
}

/// Text content part.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionContentPartText {
    #[serde(rename = "type")]
    part_type: ChatCompletionContentPartTextType,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    // TODO: Harden this Value-backed extension map to concrete text content-part fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionContentPartText {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            part_type: ChatCompletionContentPartTextType::Text,
            text: text.into(),
            cache_control: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ChatCompletionContentPartImageType {
    ImageUrl,
}

/// Image content part.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionContentPartImage {
    #[serde(rename = "type")]
    part_type: ChatCompletionContentPartImageType,
    pub image_url: ChatCompletionImageUrl,
    // TODO: Harden this Value-backed extension map to concrete image content-part fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionContentPartImage {
    pub fn new(image_url: ChatCompletionImageUrl) -> Self {
        Self {
            part_type: ChatCompletionContentPartImageType::ImageUrl,
            image_url,
            extra: Map::new(),
        }
    }
}

/// Image URL payload.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ChatCompletionImageDetail>,
    // TODO: Harden this Value-backed extension map to concrete image URL fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionImageUrl {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            detail: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChatCompletionImageDetail {
    Auto,
    Low,
    High,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ChatCompletionContentPartFileType {
    File,
}

/// File content part.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionContentPartFile {
    #[serde(rename = "type")]
    part_type: ChatCompletionContentPartFileType,
    pub file: ChatCompletionContentPartFileFile,
    // TODO: Harden this Value-backed extension map to concrete file content-part fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionContentPartFile {
    pub fn new(file: ChatCompletionContentPartFileFile) -> Self {
        Self {
            part_type: ChatCompletionContentPartFileType::File,
            file,
            extra: Map::new(),
        }
    }
}

/// File payload for file content parts.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionContentPartFileFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete file payload fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Cache-control metadata for content parts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: CacheControlType,
    // TODO: Harden this Value-backed extension map to concrete cache-control fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            cache_type: CacheControlType::Ephemeral,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CacheControlType {
    Ephemeral,
}

/// Function-call payload used by assistant messages and tool calls.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionFunctionCall {
    pub arguments: String,
    pub name: String,
    // TODO: Harden this Value-backed extension map to concrete function-call fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionFunctionCall {
    pub fn new(name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            arguments: arguments.into(),
            name: name.into(),
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ChatCompletionMessageToolCallType {
    Function,
}

/// Tool call emitted by an assistant message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionMessageToolCall {
    pub id: String,
    pub function: ChatCompletionFunctionCall,
    #[serde(rename = "type")]
    call_type: ChatCompletionMessageToolCallType,
    // TODO: Harden this Value-backed extension map to concrete tool-call fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionMessageToolCall {
    pub fn function(id: impl Into<String>, function: ChatCompletionFunctionCall) -> Self {
        Self {
            id: id.into(),
            function,
            call_type: ChatCompletionMessageToolCallType::Function,
            extra: Map::new(),
        }
    }
}

/// Reasoning metadata attached to assistant messages.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionMessageReasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete reasoning fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// System message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionSystemMessage {
    pub content: ChatCompletionTextContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete system message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionSystemMessage {
    pub fn new(content: impl Into<ChatCompletionTextContent>) -> Self {
        Self {
            content: content.into(),
            name: None,
            extra: Map::new(),
        }
    }
}

/// Developer message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionDeveloperMessage {
    pub content: ChatCompletionTextContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete developer message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionDeveloperMessage {
    pub fn new(content: impl Into<ChatCompletionTextContent>) -> Self {
        Self {
            content: content.into(),
            name: None,
            extra: Map::new(),
        }
    }
}

/// User message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionUserMessage {
    pub content: ChatCompletionContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete user message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionUserMessage {
    pub fn new(content: impl Into<ChatCompletionContent>) -> Self {
        Self {
            content: content.into(),
            name: None,
            extra: Map::new(),
        }
    }
}

/// Assistant message.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionAssistantMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ChatCompletionAssistantContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<ChatCompletionFunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatCompletionMessageToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Vec<ChatCompletionMessageReasoning>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete assistant message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionAssistantMessage {
    pub fn new(content: Option<ChatCompletionAssistantContent>) -> Self {
        Self {
            content,
            ..Default::default()
        }
    }
}

/// Tool message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionToolMessage {
    pub content: ChatCompletionTextContent,
    pub tool_call_id: String,
    // TODO: Harden this Value-backed extension map to concrete tool message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionToolMessage {
    pub fn new(
        content: impl Into<ChatCompletionTextContent>,
        tool_call_id: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            tool_call_id: tool_call_id.into(),
            extra: Map::new(),
        }
    }
}

/// Function message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionFunctionMessage {
    pub content: Option<String>,
    pub name: String,
    // TODO: Harden this Value-backed extension map to concrete function message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionFunctionMessage {
    pub fn new(name: impl Into<String>, content: Option<String>) -> Self {
        Self {
            content,
            name: name.into(),
            extra: Map::new(),
        }
    }
}

/// Fallback message for Braintrust-supported non-OpenAI roles such as `model`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ChatCompletionFallbackMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ChatCompletionNullableString>,
    // TODO: Harden this Value-backed extension map to concrete fallback message fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ChatCompletionFallbackMessage {
    pub fn new(content: Option<ChatCompletionNullableString>) -> Self {
        Self {
            content,
            extra: Map::new(),
        }
    }
}

/// Prompt block data.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PromptBlockData {
    Chat {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<ChatCompletionMessage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<String>,
    },
    Completion {
        content: String,
    },
}

/// Prompt model options.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PromptOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    // TODO: Harden this Value-backed provider parameter map to known provider-specific params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete prompt option fields.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Parser configuration for prompt outputs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PromptParser {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub parser_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_cot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_scores: Option<BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_no_match: Option<bool>,
    // TODO: Harden this Value-backed extension map to concrete parser variants.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Prompt origin information.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PromptOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
}

/// Prompt data stored on prompts and prompt-backed functions.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PromptData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptBlockData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<PromptOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser: Option<PromptParser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_functions: Option<Vec<SavedFunctionId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_format: Option<TemplateFormat>,
    // TODO: Harden this Value-backed MCP map to concrete MCP server reference variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PromptOrigin>,
    // TODO: Harden this Value-backed extension map to concrete prompt data variants.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Backend-managed function data.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FunctionData {
    Prompt,
    Code {
        // TODO: Harden this Value-backed code data to inline and bundle code variants.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    Graph {
        // TODO: Harden this Value-backed graph data to the concrete graph schema.
        #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
        extra: Map<String, Value>,
    },
    RemoteEval {
        endpoint: String,
        eval_name: String,
        // TODO: Harden this Value-backed parameter map to concrete remote eval params.
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        parameters: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameters_version: Option<String>,
    },
    Global {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        function_type: Option<FunctionType>,
        // TODO: Harden this Value-backed global function config to concrete global config variants.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Map<String, Value>>,
    },
    Facet {
        // TODO: Harden this Value-backed facet data to the concrete facet schema.
        #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
        extra: Map<String, Value>,
    },
    BatchedFacet {
        // TODO: Harden this Value-backed batched facet data to the concrete batched facet schema.
        #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
        extra: Map<String, Value>,
    },
    Parameters {
        // TODO: Harden this Value-backed parameter payload to the concrete parameters schema.
        data: Value,
        #[serde(rename = "__schema")]
        schema: ParameterSchema,
    },
    TopicMap {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report_key: Option<String>,
        // TODO: Harden this Value-backed topic map extension map to concrete topic map fields.
        #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
        extra: Map<String, Value>,
    },
    #[serde(other)]
    Unknown,
}

/// JSON object schema used by parameter functions.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ParameterSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    // TODO: Harden this Value-backed properties map to JSON Schema property definitions.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub properties: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<bool>,
}

/// Function output schema.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct FunctionSchema {
    // TODO: Harden this Value-backed schema field to JSON Schema parameter definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    // TODO: Harden this Value-backed schema field to JSON Schema return definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Value>,
}

/// Origin object attached to functions.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct FunctionOrigin {
    pub object_type: String,
    pub object_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal: Option<bool>,
}

/// Function object returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Function {
    pub id: String,
    #[serde(rename = "_xact_id", default, skip_serializing_if = "Option::is_none")]
    pub xact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_data: Option<PromptData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_type: Option<FunctionType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_data: Option<FunctionData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<FunctionOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_schema: Option<FunctionSchema>,
    // TODO: Harden this Value-backed extension map to concrete function fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Function {
    api_getters! {
        str id;
        option_str xact_id;
        option_str project_id;
        option_str log_id;
        option_str org_id;
        option_str name;
        option_str slug;
        option_str description;
        option_str created;
        option_ref prompt_data: PromptData;
        option_slice tags: String;
        option_map metadata;
        option_ref function_type: FunctionType;
        option_ref function_data: FunctionData;
        option_ref origin: FunctionOrigin;
        option_ref function_schema: FunctionSchema;
        map extra;
    }
}

/// Prompt object returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Prompt {
    pub id: String,
    #[serde(rename = "_xact_id", default, skip_serializing_if = "Option::is_none")]
    pub xact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_data: Option<PromptData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_type: Option<FunctionType>,
    // TODO: Harden this Value-backed extension map to concrete prompt fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Prompt {
    api_getters! {
        str id;
        option_str xact_id;
        option_str project_id;
        option_str log_id;
        option_str org_id;
        option_str name;
        option_str slug;
        option_str description;
        option_str created;
        option_ref prompt_data: PromptData;
        option_slice tags: String;
        option_map metadata;
        option_ref function_type: FunctionType;
        map extra;
    }
}

/// Parent row reference used for function invocation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum InvokeParent {
    Object(InvokeParentObject),
    Id(String),
}

/// Object-shaped invocation parent.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct InvokeParentObject {
    pub object_type: String,
    pub object_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_ids: Option<InvokeParentRowIds>,
    // TODO: Harden this Value-backed propagated event to the concrete event schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagated_event: Option<Map<String, Value>>,
}

/// Row ids attached to an invocation parent.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct InvokeParentRowIds {
    pub id: String,
    pub span_id: String,
    pub root_span_id: String,
}

/// MCP auth token payload keyed by server id.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct McpAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn chat_completion_system_message_serializes_text_content() {
        let message = ChatCompletionMessage::system("You are helpful");

        let body = serde_json::to_value(message).expect("serialize message");

        assert_eq!(
            body,
            json!({
                "role": "system",
                "content": "You are helpful"
            })
        );
    }

    #[test]
    fn chat_completion_user_message_supports_mixed_content_parts() {
        let mut image_url = ChatCompletionImageUrl::new("https://example.com/image.png");
        image_url.detail = Some(ChatCompletionImageDetail::High);

        let parts: Vec<ChatCompletionContentPart> = vec![
            ChatCompletionContentPartText::new("Describe this").into(),
            ChatCompletionContentPartImage::new(image_url).into(),
            ChatCompletionContentPartFile::new(ChatCompletionContentPartFileFile {
                file_id: Some("file-123".to_string()),
                filename: Some("brief.pdf".to_string()),
                ..Default::default()
            })
            .into(),
        ];
        let message = ChatCompletionMessage::user(parts);

        let body = serde_json::to_value(message).expect("serialize message");

        assert_eq!(
            body,
            json!({
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "Describe this"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/image.png",
                            "detail": "high"
                        }
                    },
                    {
                        "type": "file",
                        "file": {
                            "filename": "brief.pdf",
                            "file_id": "file-123"
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn chat_completion_assistant_message_supports_tools_and_reasoning() {
        let mut message =
            ChatCompletionAssistantMessage::new(Some(ChatCompletionAssistantContent::Null));
        message.function_call = Some(ChatCompletionFunctionCall::new("legacy_call", "{}"));
        message.tool_calls = Some(vec![ChatCompletionMessageToolCall::function(
            "call-123",
            ChatCompletionFunctionCall::new("lookup_weather", "{\"city\":\"SF\"}"),
        )]);
        message.reasoning = Some(vec![ChatCompletionMessageReasoning {
            id: Some("reasoning-1".to_string()),
            content: Some("Need current weather.".to_string()),
            ..Default::default()
        }]);
        message.reasoning_signature = Some("signature".to_string());

        let body = serde_json::to_value(ChatCompletionMessage::Assistant(message))
            .expect("serialize message");

        assert_eq!(
            body,
            json!({
                "role": "assistant",
                "content": null,
                "function_call": {
                    "name": "legacy_call",
                    "arguments": "{}"
                },
                "tool_calls": [{
                    "id": "call-123",
                    "type": "function",
                    "function": {
                        "name": "lookup_weather",
                        "arguments": "{\"city\":\"SF\"}"
                    }
                }],
                "reasoning": [{
                    "id": "reasoning-1",
                    "content": "Need current weather."
                }],
                "reasoning_signature": "signature"
            })
        );
    }

    #[test]
    fn chat_completion_tool_function_and_fallback_messages_serialize() {
        let tool = serde_json::to_value(ChatCompletionMessage::tool(
            vec![ChatCompletionContentPartText::new("tool result")],
            "call-123",
        ))
        .expect("serialize tool message");
        assert_eq!(
            tool,
            json!({
                "role": "tool",
                "content": [{
                    "type": "text",
                    "text": "tool result"
                }],
                "tool_call_id": "call-123"
            })
        );

        let function = serde_json::to_value(ChatCompletionMessage::function("legacy_tool", None))
            .expect("serialize function message");
        assert_eq!(
            function,
            json!({
                "role": "function",
                "content": null,
                "name": "legacy_tool"
            })
        );

        let model = serde_json::to_value(ChatCompletionMessage::model(Some(
            ChatCompletionNullableString::Null,
        )))
        .expect("serialize model message");
        assert_eq!(
            model,
            json!({
                "role": "model",
                "content": null
            })
        );
    }

    #[test]
    fn chat_completion_messages_deserialize_and_preserve_extras() {
        let message: ChatCompletionMessage = serde_json::from_value(json!({
            "role": "user",
            "name": "caller",
            "content": [{
                "type": "text",
                "text": "hello",
                "provider_part": true
            }],
            "provider_message": "extra"
        }))
        .expect("deserialize user message");

        let ChatCompletionMessage::User(user) = message else {
            panic!("expected user message");
        };
        assert_eq!(user.name.as_deref(), Some("caller"));
        assert_eq!(user.extra["provider_message"], "extra");

        let ChatCompletionContent::Parts(parts) = user.content else {
            panic!("expected content parts");
        };
        let ChatCompletionContentPart::Text(text) = &parts[0] else {
            panic!("expected text part");
        };
        assert_eq!(text.text, "hello");
        assert_eq!(text.extra["provider_part"], true);
    }
}
