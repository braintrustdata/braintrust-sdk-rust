use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::Result;
use crate::types::{ParentSpanInfo, SpanPayload};

/// Event data to log to a span. All fields are optional.
/// Multiple calls to `log()` will merge data.
#[derive(Clone, Default)]
pub struct SpanLog {
    pub name: Option<String>,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub metadata: Option<Map<String, Value>>,
    pub metrics: Option<HashMap<String, f64>>,
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
    }

    /// Flush span data to Braintrust. Can be called multiple times - last writer wins.
    /// Each call updates the same span (same row_id and span_id).
    /// First flush sends with is_merge=false (replace), subsequent flushes send is_merge=true (merge).
    pub async fn flush(&self) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Capture end time and add start/end to metrics
        let end_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .ok();

        let payload: SpanPayload = {
            let mut inner = self.inner.lock().await;
            if let Some(start) = inner.start_time {
                inner.metrics.insert("start".to_string(), start);
            }
            if let Some(end) = end_time {
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
    input: Option<Value>,
    output: Option<Value>,
    metadata: Map<String, Value>,
    metrics: HashMap<String, f64>,
    start_time: Option<f64>,
}

impl From<SpanData> for SpanPayload {
    fn from(data: SpanData) -> Self {
        let mut span_attributes = Map::new();
        if let Some(name) = data.name {
            span_attributes.insert("name".to_string(), Value::String(name));
        }
        span_attributes.insert("type".to_string(), Value::String("llm".to_string()));

        Self {
            row_id: data.row_id,
            span_id: data.span_id,
            is_merge: data.has_flushed, // First flush = false (replace), subsequent = true (merge)
            org_id: data.org_id,
            org_name: data.org_name,
            project_name: data.project_name,
            name: None,
            input: data.input,
            output: data.output,
            metadata: (!data.metadata.is_empty()).then_some(data.metadata),
            metrics: (!data.metrics.is_empty()).then_some(data.metrics),
            span_attributes: (!span_attributes.is_empty()).then_some(span_attributes),
        }
    }
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
        span.log(SpanLog {
            input: Some(json!({"input": "hello"})),
            output: Some(json!({"output": "world"})),
            ..Default::default()
        })
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
        span.log(SpanLog {
            metadata: Some([("foo".to_string(), json!("bar"))].into_iter().collect()),
            metrics: Some(usage_metrics_to_map(UsageMetrics {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                reasoning_tokens: None,
                ..Default::default()
            })),
            ..Default::default()
        })
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
        span.log(SpanLog {
            input: Some(json!("data")),
            ..Default::default()
        })
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
}
