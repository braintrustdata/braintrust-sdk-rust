//! Normalized changes applied to retained output, without wire serialization.
use crate::UsageMetrics;
#[derive(Default)]
pub(super) struct Update {
    pub model: Option<String>,
    pub usage: Option<UsageMetrics>,
    pub choices: Vec<Choice>,
}
pub(super) struct Choice {
    pub finish_reason: Option<String>,
    pub delta: Option<Delta>,
}
pub(super) struct Delta {
    pub role: Option<String>,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolDelta>>,
}
pub(super) struct ToolDelta {
    pub index: Option<usize>,
    pub id: Option<String>,
    pub call_type: Option<String>,
    pub function: Option<FunctionDelta>,
}
pub(super) struct FunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}
