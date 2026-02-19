use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::Result;
use crate::types::{ParentSpanInfo, SpanAttributes, SpanPayload, SpanType};

/// Error type for SpanLog builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SpanLogBuilderError {
    /// A score value was outside the valid range (0.0 to 1.0).
    ScoreOutOfRange { key: String, value: f64 },
    /// A tag was empty or invalid.
    InvalidTag { index: usize, reason: String },
    /// A metric key was empty.
    EmptyMetricKey,
    /// A score key was empty.
    EmptyScoreKey,
}

impl fmt::Display for SpanLogBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScoreOutOfRange { key, value } => {
                write!(
                    f,
                    "score '{}' has value {} which is outside the valid range [0.0, 1.0]",
                    key, value
                )
            }
            Self::InvalidTag { index, reason } => {
                write!(f, "tag at index {} is invalid: {}", index, reason)
            }
            Self::EmptyMetricKey => write!(f, "metric key cannot be empty"),
            Self::EmptyScoreKey => write!(f, "score key cannot be empty"),
        }
    }
}

impl std::error::Error for SpanLogBuilderError {}

/// Event data to log to a span. All fields are optional.
/// Multiple calls to `log()` will merge data.
///
/// Use `SpanLog::builder()` to construct instances.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct SpanLog {
    pub(crate) name: Option<String>,
    pub(crate) input: Option<Value>,
    pub(crate) output: Option<Value>,
    /// Expected output for comparison (used in evaluations).
    pub(crate) expected: Option<Value>,
    /// Error that occurred during execution.
    pub(crate) error: Option<Value>,
    /// Score values (0-1 range recommended) for evaluation.
    pub(crate) scores: Option<HashMap<String, f64>>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) metrics: Option<HashMap<String, f64>>,
    /// Tags for categorization.
    pub(crate) tags: Option<Vec<String>>,
    /// Arbitrary context data.
    pub(crate) context: Option<Value>,
}

impl SpanLog {
    /// Create a new SpanLog builder.
    pub fn builder() -> SpanLogBuilder {
        SpanLogBuilder::new()
    }
}

/// Builder for SpanLog.
#[derive(Clone, Default)]
pub struct SpanLogBuilder {
    inner: SpanLog,
}

impl SpanLogBuilder {
    /// Create a new SpanLogBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the span name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.inner.name = Some(name.into());
        self
    }

    /// Set the input data.
    pub fn input(mut self, input: impl Into<Value>) -> Self {
        self.inner.input = Some(input.into());
        self
    }

    /// Set the output data.
    pub fn output(mut self, output: impl Into<Value>) -> Self {
        self.inner.output = Some(output.into());
        self
    }

    /// Set the expected output for comparison (used in evaluations).
    pub fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.inner.expected = Some(expected.into());
        self
    }

    /// Set the error that occurred during execution.
    pub fn error(mut self, error: impl Into<Value>) -> Self {
        self.inner.error = Some(error.into());
        self
    }

    /// Set multiple scores at once.
    pub fn scores(mut self, scores: HashMap<String, f64>) -> Self {
        self.inner.scores = Some(scores);
        self
    }

    /// Add a single score (0-1 range recommended).
    pub fn score(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .scores
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.inner.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner
            .metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set the metrics map.
    pub fn metrics(mut self, metrics: HashMap<String, f64>) -> Self {
        self.inner.metrics = Some(metrics);
        self
    }

    /// Add a single metric.
    pub fn metric(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .metrics
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Add a single tag for categorization.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.inner
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Set arbitrary context data.
    pub fn context(mut self, context: impl Into<Value>) -> Self {
        self.inner.context = Some(context.into());
        self
    }

    /// Build the SpanLog.
    ///
    /// Currently this always succeeds, but returns a `Result` to allow
    /// for future validation (e.g., score range checking) without breaking changes.
    pub fn build(self) -> std::result::Result<SpanLog, SpanLogBuilderError> {
        // Future validation can be added here, e.g.:
        // - Validate scores are in 0-1 range
        // - Validate tags are non-empty
        // - Validate metric/score keys are non-empty
        Ok(self.inner)
    }
}

#[async_trait]
pub(crate) trait SpanSubmitter: Send + Sync {
    async fn submit(
        &self,
        token: impl Into<String> + Send,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()>;
}

#[allow(private_bounds)]
pub struct SpanBuilder<S: SpanSubmitter> {
    submitter: Arc<S>,
    token: String,
    org_id: String,
    org_name: Option<String>,
    project_name: Option<String>,
    parent_info: Option<ParentSpanInfo>,
    span_type: SpanType,
    purpose: Option<String>,
}

impl<S: SpanSubmitter> Clone for SpanBuilder<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            org_id: self.org_id.clone(),
            org_name: self.org_name.clone(),
            project_name: self.project_name.clone(),
            parent_info: self.parent_info.clone(),
            span_type: self.span_type,
            purpose: self.purpose.clone(),
        }
    }
}

#[allow(private_bounds)]
impl<S: SpanSubmitter> SpanBuilder<S> {
    pub(crate) fn new(
        submitter: Arc<S>,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> Self {
        Self {
            submitter,
            token: token.into(),
            org_id: org_id.into(),
            org_name: None,
            project_name: None,
            parent_info: None,
            span_type: SpanType::default(),
            purpose: None,
        }
    }

    pub fn org_name(mut self, org_name: impl Into<String>) -> Self {
        self.org_name = Some(org_name.into());
        self
    }

    pub fn project_name(mut self, project_name: impl Into<String>) -> Self {
        self.project_name = Some(project_name.into());
        self
    }

    pub fn parent_info(mut self, parent: ParentSpanInfo) -> Self {
        self.parent_info = Some(parent);
        self
    }

    pub fn span_type(mut self, span_type: SpanType) -> Self {
        self.span_type = span_type;
        self
    }

    pub fn purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    pub fn build(self) -> SpanHandle<S> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Generate both IDs ONCE at span creation - reused for all flushes
        let row_id = Uuid::new_v4().to_string();
        let span_id = Uuid::new_v4().to_string();
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .ok();

        SpanHandle {
            submitter: Arc::clone(&self.submitter),
            token: self.token,
            parent_info: self.parent_info,
            inner: Arc::new(Mutex::new(SpanData {
                row_id,
                span_id,
                has_flushed: false,
                org_id: self.org_id,
                org_name: self.org_name,
                project_name: self.project_name,
                start_time,
                span_type: self.span_type,
                purpose: self.purpose,
                ..Default::default()
            })),
        }
    }
}

#[allow(private_bounds)]
pub struct SpanHandle<S: SpanSubmitter> {
    submitter: Arc<S>,
    token: String,
    parent_info: Option<ParentSpanInfo>,
    inner: Arc<Mutex<SpanData>>,
}

impl<S: SpanSubmitter> Clone for SpanHandle<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            parent_info: self.parent_info.clone(),
            inner: Arc::clone(&self.inner),
        }
    }
}

#[allow(private_bounds)]
impl<S: SpanSubmitter> SpanHandle<S> {
    /// Log event data to this span. All fields are optional.
    /// Multiple calls will merge data (later values overwrite earlier ones).
    pub async fn log(&self, event: SpanLog) {
        let mut inner = self.inner.lock().await;

        if let Some(name) = event.name {
            inner.name = Some(name);
        }
        if let Some(input) = event.input {
            inner.input = Some(input);
        }
        if let Some(output) = event.output {
            inner.output = Some(output);
        }
        if let Some(expected) = event.expected {
            inner.expected = Some(expected);
        }
        if let Some(error) = event.error {
            inner.error = Some(error);
        }
        if let Some(scores) = event.scores {
            for (key, value) in scores {
                inner.scores.insert(key, value);
            }
        }
        if let Some(metadata) = event.metadata {
            for (key, value) in metadata {
                inner.metadata.insert(key, value);
            }
        }
        if let Some(metrics) = event.metrics {
            for (key, value) in metrics {
                inner.metrics.insert(key, value);
            }
        }
        if let Some(tags) = event.tags {
            inner.tags.extend(tags);
        }
        if let Some(context) = event.context {
            inner.context = Some(context);
        }
    }

    /// Flush span data to Braintrust. Can be called multiple times - last writer wins.
    /// Each call updates the same span (same row_id and span_id).
    /// First flush sends with is_merge=false (replace), subsequent flushes send is_merge=true (merge).
    ///
    /// This does not mark the span as completed. Call [`SpanHandle::end`] to set
    /// the `end` metric before final flush.
    pub async fn flush(&self) -> Result<()> {
        let payload: SpanPayload = {
            let mut inner = self.inner.lock().await;
            if let Some(start) = inner.start_time {
                inner.metrics.entry("start".to_string()).or_insert(start);
            }
            if let Some(end) = inner.end_time {
                inner.metrics.insert("end".to_string(), end);
            }

            // Create payload from current state (captures has_flushed for is_merge)
            let payload: SpanPayload = inner.clone().into();

            // Mark as flushed for subsequent calls
            inner.has_flushed = true;

            payload
        };

        self.submitter
            .submit(self.token.clone(), payload, self.parent_info.clone())
            .await
    }

    /// Mark the span as ended with the current timestamp.
    ///
    /// Calling `end()` multiple times is idempotent: once an end time is set,
    /// subsequent calls return the same value without overwriting it.
    pub async fn end(&self) -> f64 {
        self.end_with_time(epoch_secs()).await
    }

    /// Mark the span as ended with an explicit timestamp (seconds since Unix epoch).
    ///
    /// Calling this multiple times is idempotent: once an end time is set,
    /// subsequent calls return the previously-set value.
    pub async fn end_with_time(&self, end_time: f64) -> f64 {
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.end_time {
            return existing;
        }
        inner.end_time = Some(end_time);
        end_time
    }
}

#[derive(Clone, Default)]
struct SpanData {
    row_id: String,
    span_id: String,
    has_flushed: bool,
    org_id: String,
    org_name: Option<String>,
    project_name: Option<String>,
    name: Option<String>,
    span_type: SpanType,
    purpose: Option<String>,
    input: Option<Value>,
    output: Option<Value>,
    expected: Option<Value>,
    error: Option<Value>,
    scores: HashMap<String, f64>,
    metadata: Map<String, Value>,
    metrics: HashMap<String, f64>,
    tags: Vec<String>,
    context: Option<Value>,
    start_time: Option<f64>,
    end_time: Option<f64>,
}

impl From<SpanData> for SpanPayload {
    fn from(data: SpanData) -> Self {
        let span_attributes = SpanAttributes {
            name: data.name,
            span_type: Some(data.span_type),
            purpose: data.purpose,
            extra: HashMap::new(),
        };

        // Only include span_attributes if it has meaningful content
        let has_attributes = span_attributes.name.is_some()
            || span_attributes.span_type.is_some()
            || span_attributes.purpose.is_some();

        Self {
            row_id: data.row_id,
            span_id: data.span_id,
            is_merge: data.has_flushed, // First flush = false (replace), subsequent = true (merge)
            org_id: data.org_id,
            org_name: data.org_name,
            project_name: data.project_name,
            input: data.input,
            output: data.output,
            expected: data.expected,
            error: data.error,
            scores: (!data.scores.is_empty()).then_some(data.scores),
            metadata: (!data.metadata.is_empty()).then_some(data.metadata),
            metrics: (!data.metrics.is_empty()).then_some(data.metrics),
            tags: (!data.tags.is_empty()).then_some(data.tags),
            context: data.context,
            span_attributes: has_attributes.then_some(span_attributes),
        }
    }
}

fn epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{build_test_span, mock_span_builder};
    use crate::types::{usage_metrics_to_map, UsageMetrics};
    use serde_json::json;

    #[tokio::test]
    async fn span_logs_input_and_output() {
        let (span, collector) = build_test_span();
        span.log(
            SpanLog::builder()
                .input(json!({"input": "hello"}))
                .output(json!({"output": "world"}))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");

        let spans = collector.spans();
        assert_eq!(spans.len(), 1);
        let captured = &spans[0];
        assert!(captured.payload.input.is_some());
        assert!(captured.payload.output.is_some());
    }

    #[tokio::test]
    async fn span_logs_metadata_and_metrics() {
        let (span, collector) = build_test_span();
        span.log(
            SpanLog::builder()
                .metadata([("foo".to_string(), json!("bar"))].into_iter().collect())
                .metrics(usage_metrics_to_map(UsageMetrics {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    total_tokens: Some(15),
                    reasoning_tokens: None,
                    ..Default::default()
                }))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        let metadata = captured.payload.metadata.unwrap();
        assert_eq!(metadata.get("foo").unwrap(), "bar");
        let metrics = captured.payload.metrics.unwrap();
        assert_eq!(metrics.get("prompt_tokens").copied(), Some(10.0));
        assert_eq!(metrics.get("completion_tokens").copied(), Some(5.0));
        assert_eq!(metrics.get("tokens").copied(), Some(15.0));
    }

    #[tokio::test]
    async fn builder_applies_project_and_parent_info() {
        let (builder, collector) = mock_span_builder();
        let span = builder
            .project_name("demo-project")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "proj-id".into(),
            })
            .build();
        span.log(
            SpanLog::builder()
                .input(json!("data"))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        assert_eq!(
            captured.payload.project_name.as_deref().expect("project"),
            "demo-project"
        );
        assert!(matches!(
            captured.parent,
            Some(ParentSpanInfo::ProjectLogs { .. })
        ));
    }

    #[tokio::test]
    async fn span_logs_scores_expected_and_tags() {
        let (span, collector) = build_test_span();
        span.log(
            SpanLog::builder()
                .output(json!({"answer": "Paris"}))
                .expected(json!({"answer": "Paris"}))
                .scores(
                    [
                        ("accuracy".to_string(), 1.0),
                        ("relevance".to_string(), 0.95),
                    ]
                    .into_iter()
                    .collect(),
                )
                .tags(vec!["geography".to_string(), "qa".to_string()])
                .context(json!({"source": "test"}))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        assert_eq!(captured.payload.expected, Some(json!({"answer": "Paris"})));
        let scores = captured.payload.scores.unwrap();
        assert_eq!(scores.get("accuracy").copied(), Some(1.0));
        assert_eq!(scores.get("relevance").copied(), Some(0.95));
        let tags = captured.payload.tags.unwrap();
        assert!(tags.contains(&"geography".to_string()));
        assert!(tags.contains(&"qa".to_string()));
        assert_eq!(captured.payload.context, Some(json!({"source": "test"})));
    }

    #[tokio::test]
    async fn span_logs_error() {
        let (span, collector) = build_test_span();
        span.log(
            SpanLog::builder()
                .error(json!({"message": "Something went wrong", "code": 500}))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        assert_eq!(
            captured.payload.error,
            Some(json!({"message": "Something went wrong", "code": 500}))
        );
    }

    #[tokio::test]
    async fn flush_does_not_set_end_metric() {
        let (span, collector) = build_test_span();
        span.log(
            SpanLog::builder()
                .input(json!({"input": "hello"}))
                .build()
                .expect("build"),
        )
        .await;
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        let metrics = captured.payload.metrics.unwrap_or_default();
        assert!(metrics.contains_key("start"));
        assert!(!metrics.contains_key("end"));
    }

    #[tokio::test]
    async fn end_sets_end_metric_on_flush() {
        let (span, collector) = build_test_span();
        span.end_with_time(123.0).await;
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        let metrics = captured.payload.metrics.unwrap_or_default();
        assert_eq!(metrics.get("end").copied(), Some(123.0));
    }

    #[tokio::test]
    async fn end_is_idempotent() {
        let (span, _collector) = build_test_span();
        let first = span.end_with_time(123.0).await;
        let second = span.end_with_time(456.0).await;
        assert_eq!(first, 123.0);
        assert_eq!(second, 123.0);
    }
}
