mod error;
mod extractors;
mod logger;
mod span;
mod stream;
#[cfg(test)]
pub(crate) mod test_utils;
mod types;

pub use error::{BraintrustError, Result};
pub use extractors::{extract_anthropic_usage, extract_openai_usage};
pub use logger::{BraintrustClient, BraintrustClientBuilder, LoginState, OrgInfo};
pub use span::{SpanBuilder, SpanHandle, SpanLog, SpanLogBuilder, SpanLogBuilderError};
pub use stream::{
    wrap_stream_with_span, BraintrustStream, ChatMessage, ChatMessageBuilder,
    ChatMessageBuilderError, FinalizedStream, FinalizedStreamBuilder, FinalizedStreamBuilderError,
    FunctionCall, FunctionCallBuilder, FunctionCallBuilderError, OutputChoice, OutputChoiceBuilder,
    OutputChoiceBuilderError, StreamMetadata, StreamMetadataBuilder, StreamMetadataBuilderError,
    ToolCall, ToolCallBuilder, ToolCallBuilderError,
};
pub use types::{
    CompletionTokensDetails, ParentSpanInfo, PromptTokensDetails, SpanType, Usage, UsageMetrics,
};
