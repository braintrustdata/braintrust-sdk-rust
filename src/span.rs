use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::error::Result;
use crate::span_components::SpanComponents;
use crate::types::{
    ParentSpanInfo, SpanAttributes, SpanEventData, SpanObjectType, SpanPayload, SpanType,
};

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
    /// Submit a span payload for queuing (fire-and-forget).
    /// The payload is queued internally and processed by a background worker.
    /// Dropping is handled internally if the queue is full.
    fn submit(&self, token: String, payload: SpanPayload, parent_info: Option<ParentSpanInfo>);

    /// Flush all queued span data through to the server.
    async fn flush(&self) -> Result<()>;

    /// Trigger a non-blocking background flush.
    async fn trigger_flush(&self) -> Result<()>;
}

#[derive(Debug)]
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
    start_time_override: Option<f64>,
    environment: Option<SpanOriginEnvironment>,
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
            start_time_override: self.start_time_override,
            environment: self.environment.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanOriginEnvironment {
    pub environment_type: String,
    pub name: Option<String>,
}

impl SpanOriginEnvironment {
    pub fn new(environment_type: impl Into<String>, name: Option<impl Into<String>>) -> Self {
        Self {
            environment_type: environment_type.into(),
            name: name.map(Into::into),
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
            start_time_override: None,
            environment: detected_environment(),
        }
    }

    pub fn start_time(mut self, start_time: f64) -> Self {
        self.start_time_override = Some(start_time);
        self
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

    pub fn environment(
        mut self,
        environment_type: impl Into<String>,
        name: Option<impl Into<String>>,
    ) -> Self {
        self.environment = Some(SpanOriginEnvironment::new(environment_type, name));
        self
    }

    pub fn build(self) -> SpanHandle<S> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Generate both IDs ONCE at span creation - reused for all flushes
        let row_id = Uuid::new_v4().to_string();
        let span_id = Uuid::new_v4().to_string();
        let start_time = self.start_time_override.or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .ok()
        });

        // Extract propagated_event from parent if available
        let propagated_event = self.parent_info.as_ref().and_then(|parent| {
            if let ParentSpanInfo::FullSpan {
                propagated_event, ..
            } = parent
            {
                propagated_event.clone()
            } else {
                None
            }
        });

        SpanHandle {
            submitter: Arc::clone(&self.submitter),
            token: self.token,
            parent_info: self.parent_info,
            row_id: row_id.clone(),
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
                environment: self.environment,
                propagated_event,
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
    /// Stable identifier set at creation, accessible without locking.
    row_id: String,
    inner: Arc<Mutex<SpanData>>,
}

impl<S: SpanSubmitter> Clone for SpanHandle<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            parent_info: self.parent_info.clone(),
            row_id: self.row_id.clone(),
            inner: Arc::clone(&self.inner),
        }
    }
}

#[allow(private_bounds)]
impl<S: SpanSubmitter> SpanHandle<S> {
    /// Return the stable row ID for this span, usable with `log_feedback()` and `update_span()`.
    pub fn row_id(&self) -> &str {
        &self.row_id
    }

    /// Log event data to this span. All fields are optional.
    /// Multiple calls will merge data (later values overwrite earlier ones).
    /// Each call synchronously queues the current span snapshot for background processing.
    pub fn log(&self, event: SpanLog) {
        let payload = {
            let mut inner = self
                .inner
                .lock()
                .expect("span mutex should not be poisoned");
            apply_span_log_to_data(&mut inner, event);
            current_span_payload(&mut inner)
        };

        self.submitter
            .submit(self.token.clone(), payload, self.parent_info.clone());
    }

    /// Flush all queued span data through to Braintrust.
    ///
    /// This does not create a new queued span update. Call [`SpanHandle::log`] or
    /// [`SpanHandle::end`] first to submit changes, then call `flush()` when you need
    /// to ensure the background queue has been processed.
    pub async fn flush(&self) -> Result<()> {
        self.submitter.flush().await
    }

    /// Mark the span as ended with the current timestamp.
    ///
    /// Calling `end()` multiple times is idempotent: once an end time is set,
    /// subsequent calls return the same value without overwriting it.
    pub fn end(&self) -> f64 {
        self.end_with_time(epoch_secs())
    }

    /// Mark the span as ended with an explicit timestamp (seconds since Unix epoch).
    ///
    /// Calling this multiple times is idempotent: once an end time is set,
    /// subsequent calls return the previously-set value.
    pub fn end_with_time(&self, end_time: f64) -> f64 {
        let (end_time, payload) = {
            let mut inner = self
                .inner
                .lock()
                .expect("span mutex should not be poisoned");
            if let Some(existing) = inner.end_time {
                return existing;
            }
            inner.end_time = Some(end_time);
            (end_time, current_span_payload(&mut inner))
        };

        self.submitter
            .submit(self.token.clone(), payload, self.parent_info.clone());
        end_time
    }

    /// Trigger a non-blocking background flush.
    /// Useful for streaming writes to get partial updates visible immediately.
    pub async fn trigger_flush(&self) -> Result<()> {
        self.submitter.trigger_flush().await
    }

    /// Export the span as SpanComponents for passing to other systems or SDKs.
    /// This creates a serializable representation that can be used as a parent span
    /// in another context (e.g., across service boundaries via HTTP headers).
    ///
    /// The exported SpanComponents includes the span's IDs and any propagated_event
    /// data that should flow to child spans.
    pub fn export(&self) -> Result<SpanComponents> {
        let inner = self
            .inner
            .lock()
            .expect("span mutex should not be poisoned");

        // Determine object_type and object_id from parent_info if available,
        // otherwise default to ProjectLogs
        let (object_type, object_id, inherited_compute_object_metadata_args) =
            match &self.parent_info {
                Some(ParentSpanInfo::Experiment { object_id }) => {
                    (SpanObjectType::Experiment, Some(object_id.clone()), None)
                }
                Some(ParentSpanInfo::ProjectLogs { object_id }) => {
                    (SpanObjectType::ProjectLogs, Some(object_id.clone()), None)
                }
                Some(ParentSpanInfo::PlaygroundLogs { object_id }) => (
                    SpanObjectType::PlaygroundLogs,
                    Some(object_id.clone()),
                    None,
                ),
                Some(ParentSpanInfo::FullSpan {
                    object_type,
                    object_id,
                    compute_object_metadata_args,
                    ..
                }) => (
                    *object_type,
                    object_id.clone(),
                    compute_object_metadata_args.clone(),
                ),
                // Default to ProjectLogs if no parent
                _ => (SpanObjectType::ProjectLogs, None, None),
            };

        // Use root_span_id from parent_info (FullSpan) if available, otherwise
        // fall back to this span's own span_id (it is the root).
        let root_span_id = match &self.parent_info {
            Some(ParentSpanInfo::FullSpan { root_span_id, .. }) => Some(root_span_id.clone()),
            _ => Some(inner.span_id.clone()),
        };
        let span_parents = match &self.parent_info {
            Some(ParentSpanInfo::FullSpan { span_id, .. }) => Some(vec![span_id.clone()]),
            _ => None,
        };

        let compute_object_metadata_args = if object_id.is_none() {
            inherited_compute_object_metadata_args.or_else(|| {
                inner.project_name.as_ref().map(|project_name| {
                    let mut args = Map::new();
                    args.insert(
                        "project_name".to_string(),
                        Value::String(project_name.clone()),
                    );
                    args
                })
            })
        } else {
            inherited_compute_object_metadata_args
        };

        Ok(SpanComponents {
            object_type,
            object_id,
            compute_object_metadata_args,
            row_id: Some(inner.row_id.clone()),
            span_id: Some(inner.span_id.clone()),
            root_span_id,
            span_parents,
            propagated_event: inner.propagated_event.clone(),
        })
    }
}

fn apply_span_log_to_data(inner: &mut SpanData, event: SpanLog) {
    macro_rules! replace_if_some {
        ($($field:ident),* $(,)?) => {
            $(
                if let Some(value) = event.$field {
                    inner.$field = Some(value);
                }
            )*
        };
    }

    macro_rules! merge_map_if_some {
        ($($field:ident),* $(,)?) => {
            $(
                if let Some(values) = event.$field {
                    inner.$field.extend(values);
                }
            )*
        };
    }

    replace_if_some!(name, input, output, expected, error, context);
    merge_map_if_some!(scores, metadata, metrics);

    if let Some(tags) = event.tags {
        inner.tags.extend(tags);
    }
}

fn current_span_payload(inner: &mut SpanData) -> SpanPayload {
    if let Some(start) = inner.start_time {
        inner.metrics.entry("start".to_string()).or_insert(start);
    }
    if let Some(end) = inner.end_time {
        inner.metrics.insert("end".to_string(), end);
    }

    let payload: SpanPayload = inner.clone().into();
    inner.has_flushed = true;
    payload
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
    environment: Option<SpanOriginEnvironment>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    propagated_event: Option<Map<String, Value>>,
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

        let mut event_data = SpanEventData {
            input: data.input,
            output: data.output,
            expected: data.expected,
            error: data.error,
            scores: (!data.scores.is_empty()).then_some(data.scores),
            metadata: (!data.metadata.is_empty()).then_some(data.metadata),
            metrics: (!data.metrics.is_empty()).then_some(data.metrics),
            tags: (!data.tags.is_empty()).then_some(data.tags),
            context: merge_span_origin_context(data.context, data.environment),
            span_attributes: has_attributes.then_some(span_attributes),
            extra: HashMap::new(),
        };

        if let Some(propagated_event) = data.propagated_event {
            event_data.apply_propagated_event(&propagated_event);
        }

        Self {
            row_id: data.row_id,
            span_id: data.span_id,
            is_merge: data.has_flushed, // First flush = false (replace), subsequent = true (merge)
            span_components: None,
            org_id: data.org_id,
            org_name: data.org_name,
            project_name: data.project_name,
            input: event_data.input,
            output: event_data.output,
            expected: event_data.expected,
            error: event_data.error,
            scores: event_data.scores,
            metadata: event_data.metadata,
            metrics: event_data.metrics,
            tags: event_data.tags,
            context: event_data.context,
            span_attributes: event_data.span_attributes,
            extra: event_data.extra,
        }
    }
}

pub(crate) fn merge_span_origin_context(
    context: Option<Value>,
    environment: Option<SpanOriginEnvironment>,
) -> Option<Value> {
    let mut base = context.unwrap_or_else(|| json!({}));
    let Some(obj) = base.as_object_mut() else {
        return Some(base);
    };

    let span_origin = obj
        .entry("span_origin")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(span_origin) = span_origin else {
        return Some(base);
    };

    span_origin
        .entry("name")
        .or_insert_with(|| json!("braintrust.sdk.rust"));
    span_origin
        .entry("version")
        .or_insert_with(|| json!(env!("CARGO_PKG_VERSION")));
    span_origin
        .entry("instrumentation")
        .or_insert_with(|| json!({ "name": "braintrust-rust-sdk" }));

    if !span_origin.contains_key("environment") {
        if let Some(environment) = environment {
            let mut env_obj = Map::new();
            env_obj.insert(
                "type".to_string(),
                Value::String(environment.environment_type),
            );
            if let Some(name) = environment.name {
                env_obj.insert("name".to_string(), Value::String(name));
            }
            span_origin.insert("environment".to_string(), Value::Object(env_obj));
        }
    }

    Some(base)
}

fn detected_environment() -> Option<SpanOriginEnvironment> {
    if let Some(environment_type) = env_value("BRAINTRUST_ENVIRONMENT_TYPE") {
        let name = env_value("BRAINTRUST_ENVIRONMENT_NAME");
        return Some(SpanOriginEnvironment {
            environment_type,
            name,
        });
    }

    let ci_name = [
        ("GITHUB_ACTIONS", "github_actions"),
        ("GITLAB_CI", "gitlab_ci"),
        ("CIRCLECI", "circleci"),
        ("BUILDKITE", "buildkite"),
        ("JENKINS_URL", "jenkins"),
        ("JENKINS_HOME", "jenkins"),
        ("TF_BUILD", "azure_pipelines"),
        ("TEAMCITY_VERSION", "teamcity"),
        ("TRAVIS", "travis"),
        ("BITBUCKET_BUILD_NUMBER", "bitbucket"),
    ]
    .into_iter()
    .find_map(|(var, name)| std::env::var(var).ok().map(|_| name.to_string()))
    .or_else(|| std::env::var("CI").ok().map(|_| "ci".to_string()));
    if let Some(name) = ci_name {
        return Some(SpanOriginEnvironment {
            environment_type: "ci".to_string(),
            name: Some(name),
        });
    }

    let server_name = [
        ("VERCEL", "vercel"),
        ("NETLIFY", "netlify"),
        ("AWS_LAMBDA_FUNCTION_NAME", "aws_lambda"),
        ("AWS_EXECUTION_ENV", "aws_lambda"),
        ("K_SERVICE", "cloud_run"),
        ("FUNCTION_TARGET", "gcp_functions"),
        ("KUBERNETES_SERVICE_HOST", "kubernetes"),
        ("ECS_CONTAINER_METADATA_URI", "ecs"),
        ("ECS_CONTAINER_METADATA_URI_V4", "ecs"),
        ("DYNO", "heroku"),
        ("FLY_APP_NAME", "fly"),
        ("RAILWAY_ENVIRONMENT", "railway"),
        ("RENDER_SERVICE_NAME", "render"),
    ]
    .into_iter()
    .find_map(|(var, name)| std::env::var(var).ok().map(|_| name.to_string()));
    if let Some(name) = server_name {
        return Some(SpanOriginEnvironment {
            environment_type: "server".to_string(),
            name: Some(name),
        });
    }

    None
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| braintrust_env_file_value(key))
}

fn braintrust_env_file_value(key: &str) -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    for _ in 0..=64 {
        let path: PathBuf = dir.join(".env.braintrust");
        if path.is_file() {
            let contents = fs::read_to_string(path).ok()?;
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let Some((name, value)) = trimmed.split_once('=') else {
                    continue;
                };
                if name.trim() == key {
                    return Some(
                        value
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string(),
                    )
                    .filter(|value| !value.is_empty());
                }
            }
            return None;
        }
        if !dir.pop() {
            break;
        }
    }
    None
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
    use crate::ProjectLogsIdentifier;
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
        );
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
        );
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
        );
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
        );
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        assert_eq!(captured.payload.expected, Some(json!({"answer": "Paris"})));
        let scores = captured.payload.scores.unwrap();
        assert_eq!(scores.get("accuracy").copied(), Some(1.0));
        assert_eq!(scores.get("relevance").copied(), Some(0.95));
        let tags = captured.payload.tags.unwrap();
        assert!(tags.contains(&"geography".to_string()));
        assert!(tags.contains(&"qa".to_string()));
        assert_eq!(
            captured.payload.context,
            Some(json!({
                "source": "test",
                "span_origin": {
                    "name": "braintrust.sdk.rust",
                    "version": env!("CARGO_PKG_VERSION"),
                    "instrumentation": {
                        "name": "braintrust-rust-sdk",
                    },
                },
            }))
        );
    }

    #[tokio::test]
    async fn span_logs_error() {
        let (span, collector) = build_test_span();
        span.log(
            SpanLog::builder()
                .error(json!({"message": "Something went wrong", "code": 500}))
                .build()
                .expect("build"),
        );
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
        );
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        let metrics = captured.payload.metrics.unwrap_or_default();
        assert!(metrics.contains_key("start"));
        assert!(!metrics.contains_key("end"));
    }

    #[tokio::test]
    async fn end_sets_end_metric_on_flush() {
        let (span, collector) = build_test_span();
        span.end_with_time(123.0);
        span.flush().await.expect("flush");

        let captured = collector.spans().into_iter().next().unwrap();
        let metrics = captured.payload.metrics.unwrap_or_default();
        assert_eq!(metrics.get("end").copied(), Some(123.0));
    }

    #[tokio::test]
    async fn end_is_idempotent() {
        let (span, _collector) = build_test_span();
        let first = span.end_with_time(123.0);
        let second = span.end_with_time(456.0);
        assert_eq!(first, 123.0);
        assert_eq!(second, 123.0);
    }

    #[tokio::test]
    async fn propagated_event_flows_to_child_span() {
        use crate::types::SpanObjectType;

        // Create a parent span with propagated_event
        let mut parent_propagated = Map::new();
        parent_propagated.insert("parent_key".to_string(), json!("parent_value"));
        parent_propagated.insert("metadata".to_string(), json!({"inherited": "data"}));
        parent_propagated.insert(
            "span_attributes".to_string(),
            json!({"purpose": "scorer", "skip_realtime": true}),
        );

        let parent_info = ParentSpanInfo::FullSpan {
            object_type: SpanObjectType::ProjectLogs,
            object_id: Some("project-123".to_string()),
            span_id: "parent-span-id".to_string(),
            root_span_id: "root-span-id".to_string(),
            compute_object_metadata_args: None,
            span_parents: None,
            propagated_event: Some(parent_propagated),
        };

        // Build child span with parent
        let (builder, collector) = mock_span_builder();
        let span = builder.parent_info(parent_info).build();

        // Log to the child span
        span.log(
            SpanLog::builder()
                .input(json!({"child": "input"}))
                .build()
                .expect("build"),
        );

        span.flush().await.expect("flush");

        // Verify propagated_event data was merged into the span
        let captured = collector.spans().into_iter().next().unwrap();

        // Check that metadata from propagated_event is present
        let metadata = captured.payload.metadata.as_ref().unwrap();
        assert_eq!(
            metadata.get("inherited").and_then(|v| v.as_str()),
            Some("data"),
            "Inherited metadata should be present"
        );
        assert_eq!(
            captured
                .payload
                .extra
                .get("parent_key")
                .and_then(|v| v.as_str()),
            Some("parent_value"),
            "Unknown propagated fields should remain top-level extras"
        );
        let span_attributes = captured.payload.span_attributes.as_ref().unwrap();
        assert_eq!(span_attributes.purpose.as_deref(), Some("scorer"));
        assert_eq!(
            span_attributes.extra.get("skip_realtime"),
            Some(&json!(true))
        );
    }

    #[tokio::test]
    async fn span_export_includes_propagated_event() {
        use crate::types::SpanObjectType;

        // Create a span with propagated_event
        let mut propagated = Map::new();
        propagated.insert("test_key".to_string(), json!("test_value"));

        let parent_info = ParentSpanInfo::FullSpan {
            object_type: SpanObjectType::Experiment,
            object_id: Some("exp-123".to_string()),
            span_id: "span-456".to_string(),
            root_span_id: "root-789".to_string(),
            compute_object_metadata_args: None,
            span_parents: None,
            propagated_event: Some(propagated),
        };

        let (builder, _collector) = mock_span_builder();
        let span = builder.parent_info(parent_info).build();

        // Export the span
        let exported = span.export().unwrap();

        // Verify exported SpanComponents has propagated_event
        assert!(exported.propagated_event.is_some());
        let event = exported.propagated_event.unwrap();
        assert_eq!(
            event.get("test_key").and_then(|v| v.as_str()),
            Some("test_value")
        );
        assert_eq!(exported.span_parents, Some(vec!["span-456".to_string()]));
    }

    #[tokio::test]
    async fn span_export_includes_compute_object_metadata_args_for_project_logs() {
        let (builder, _collector) = mock_span_builder();
        let span = builder.project_name("demo-project").build();

        let exported = span.export().unwrap();

        assert_eq!(exported.object_type, SpanObjectType::ProjectLogs);
        assert!(exported.object_id.is_none());
        assert_eq!(
            exported.project_logs_identifier(),
            Some(ProjectLogsIdentifier::ProjectName(
                "demo-project".to_string()
            ))
        );
    }
}
