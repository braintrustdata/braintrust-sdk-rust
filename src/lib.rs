mod dataset;
mod error;
mod experiment;
mod extractors;
mod logger;
mod logs3;
mod span;
mod stream;
#[cfg(test)]
pub(crate) mod test_utils;
mod types;

pub use dataset::{
    Dataset, DatasetBuilder, DatasetBuilderError, DatasetInsert, DatasetInsertBuilder,
    DatasetInsertBuilderError, DatasetIterator, DatasetRecord, DatasetSummary,
};
pub use error::{BraintrustError, Result};
pub use experiment::{
    BaseExperimentInfo, Experiment, ExperimentBuilder, ExperimentBuilderError, ExperimentIterator,
    ExperimentLog, ExperimentLogBuilder, ExperimentLogBuilderError, ExperimentRecord,
    ExperimentSpanBuilder, ExperimentSummary, ExportedExperiment, Feedback, FeedbackBuilder,
    FeedbackBuilderError, GitMetadataCollect, GitMetadataField, GitMetadataSettings, MetricSummary,
    ProjectMetadata, ReadonlyExperiment, RepoInfo, ScoreSummary,
};
pub use extractors::{extract_anthropic_usage, extract_openai_usage};
pub use logger::{
    BraintrustClient, BraintrustClientBuilder, LoginState, OrgInfo, DEFAULT_API_URL,
    DEFAULT_APP_URL,
};
pub use logs3::{Logs3BatchUploader, Logs3UploadResult};
pub use span::{SpanBuilder, SpanHandle, SpanLog, SpanLogBuilder, SpanLogBuilderError};
pub use stream::{
    wrap_stream_with_span, BraintrustStream, ChatMessage, ChatMessageBuilder,
    ChatMessageBuilderError, FinalizedStream, FinalizedStreamBuilder, FinalizedStreamBuilderError,
    FunctionCall, FunctionCallBuilder, FunctionCallBuilderError, OutputChoice, OutputChoiceBuilder,
    OutputChoiceBuilderError, StreamMetadata, StreamMetadataBuilder, StreamMetadataBuilderError,
    ToolCall, ToolCallBuilder, ToolCallBuilderError,
};
pub use types::{
    CompletionTokensDetails, InvalidSpanObjectType, ParentSpanInfo, PromptTokensDetails,
    SpanObjectType, SpanType, Usage, UsageMetrics,
};
