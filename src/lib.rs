mod error;
mod extractors;
mod logger;
mod prompt;
mod span;
mod stream;
#[cfg(test)]
pub(crate) mod test_utils;
mod types;

pub use error::{BraintrustError, Result};
pub use extractors::{extract_anthropic_usage, extract_openai_usage};
pub use logger::{BraintrustClient, BraintrustClientBuilder, LoginState, OrgInfo};
pub use prompt::{
    ChatPrompt, CompletionPrompt, ContentPart, ImageUrlConfig, MessageContent, ModelParams, Prompt,
    PromptBuildResult, PromptBuilder, PromptBuilderError, PromptChatMessage, PromptData,
    PromptFunctionCall, PromptFunctionDef, PromptOptions, PromptTool, PromptToolCall, PromptType,
    RenderedPrompt, ResponseFormat,
};
pub use span::{SpanBuilder, SpanHandle, SpanLog};
pub use stream::{
    wrap_stream_with_span, BraintrustStream, ChatMessage, FinalizedStream, FunctionCall,
    OutputChoice, StreamMetadata, ToolCall,
};
pub use types::{
    CompletionTokensDetails, InvalidSpanObjectType, ParentSpanInfo, PromptTokensDetails,
    SpanObjectType, SpanType, Usage, UsageMetrics,
};
