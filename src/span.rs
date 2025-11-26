use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::types::{ParentSpanInfo, SpanPayload, UsageMetrics};

#[async_trait]
pub(crate) trait SpanSubmitter: Send + Sync {
    async fn submit(
        &self,
        token: String,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()>;
}

#[derive(Clone)]
pub struct SpanBuilder {
    submitter: Arc<dyn SpanSubmitter>,
    token: String,
    org_id: String,
    org_name: Option<String>,
    project_name: Option<String>,
    parent_info: Option<ParentSpanInfo>,
}

impl SpanBuilder {
    pub(crate) fn new(
        submitter: Arc<dyn SpanSubmitter>,
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

    pub fn build(self) -> SpanHandle {
        SpanHandle {
            submitter: Arc::clone(&self.submitter),
            token: self.token,
            parent_info: self.parent_info,
            inner: Arc::new(Mutex::new(SpanData {
                org_id: self.org_id,
                org_name: self.org_name,
                project_name: self.project_name,
                ..Default::default()
            })),
        }
    }
}

#[derive(Clone)]
pub struct SpanHandle {
    submitter: Arc<dyn SpanSubmitter>,
    token: String,
    parent_info: Option<ParentSpanInfo>,
    inner: Arc<Mutex<SpanData>>,
}

impl SpanHandle {
    pub async fn set_name(&self, name: impl Into<String>) {
        self.inner.lock().await.name = Some(name.into());
    }

    pub async fn set_project_name(&self, name: impl Into<String>) {
        self.inner.lock().await.project_name = Some(name.into());
    }

    pub async fn log_input(&self, value: Value) {
        self.inner.lock().await.input = Some(value);
    }

    pub async fn log_output(&self, value: Value) {
        self.inner.lock().await.output = Some(value);
    }

    pub async fn log_metadata(&self, metadata: Map<String, Value>) {
        let mut inner = self.inner.lock().await;
        for (key, value) in metadata {
            inner.metadata.insert(key, value);
        }
    }

    pub async fn log_metrics(&self, metrics: HashMap<String, f64>) {
        let mut inner = self.inner.lock().await;
        for (key, value) in metrics {
            inner.metrics.insert(key, value);
        }
    }

    pub async fn log_usage(&self, usage: UsageMetrics) {
        let mut metrics = HashMap::new();
        insert_metric(&mut metrics, "prompt_tokens", usage.prompt_tokens);
        insert_metric(&mut metrics, "completion_tokens", usage.completion_tokens);
        // Use "tokens" to match legacy proxy - this is what the UI expects
        insert_metric(&mut metrics, "tokens", usage.total_tokens);
        insert_metric(&mut metrics, "reasoning_tokens", usage.reasoning_tokens);
        insert_metric(
            &mut metrics,
            "completion_reasoning_tokens",
            usage.completion_reasoning_tokens,
        );
        insert_metric(
            &mut metrics,
            "prompt_cached_tokens",
            usage.prompt_cached_tokens,
        );
        insert_metric(
            &mut metrics,
            "prompt_cache_creation_tokens",
            usage.prompt_cache_creation_tokens,
        );

        if let Some(details) = usage.prompt_tokens_details {
            insert_metric(&mut metrics, "prompt_audio_tokens", details.audio_tokens);
            if usage.prompt_cached_tokens.is_none() {
                insert_metric(&mut metrics, "prompt_cached_tokens", details.cached_tokens);
            }
            if usage.prompt_cache_creation_tokens.is_none() {
                insert_metric(
                    &mut metrics,
                    "prompt_cache_creation_tokens",
                    details.cache_creation_tokens,
                );
            }
        }

        if let Some(details) = usage.completion_tokens_details {
            insert_metric(
                &mut metrics,
                "completion_audio_tokens",
                details.audio_tokens,
            );
            if usage.completion_reasoning_tokens.is_none() {
                insert_metric(
                    &mut metrics,
                    "completion_reasoning_tokens",
                    details.reasoning_tokens,
                );
            }
            insert_metric(
                &mut metrics,
                "completion_accepted_prediction_tokens",
                details.accepted_prediction_tokens,
            );
            insert_metric(
                &mut metrics,
                "completion_rejected_prediction_tokens",
                details.rejected_prediction_tokens,
            );
        }

        if !metrics.is_empty() {
            self.log_metrics(metrics).await;
        }
    }

    pub async fn finish(&self) -> Result<()> {
        let payload: SpanPayload = self.inner.lock().await.clone().into();
        self.submitter
            .submit(self.token.clone(), payload, self.parent_info.clone())
            .await
    }
}

fn insert_metric(metrics: &mut HashMap<String, f64>, key: &str, value: Option<u32>) {
    if let Some(value) = value {
        metrics.insert(key.to_string(), value as f64);
    }
}

#[derive(Clone, Default)]
struct SpanData {
    org_id: String,
    org_name: Option<String>,
    project_name: Option<String>,
    name: Option<String>,
    input: Option<Value>,
    output: Option<Value>,
    metadata: Map<String, Value>,
    metrics: HashMap<String, f64>,
}

impl From<SpanData> for SpanPayload {
    fn from(data: SpanData) -> Self {
        let mut span_attributes = Map::new();
        if let Some(name) = data.name {
            span_attributes.insert("name".to_string(), Value::String(name));
        }
        span_attributes.insert("type".to_string(), Value::String("llm".to_string()));

        Self {
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
    use serde_json::json;

    use crate::test_utils::{build_test_span, mock_span_builder};

    #[tokio::test]
    async fn span_logs_input_and_output() {
        let (span, collector) = build_test_span();
        span.log_input(json!({"input": "hello"})).await;
        span.log_output(json!({"output": "world"})).await;
        span.finish().await.expect("finish");

        let spans = collector.spans();
        assert_eq!(spans.len(), 1);
        let captured = &spans[0];
        assert!(captured.payload.input.is_some());
        assert!(captured.payload.output.is_some());
    }

    #[tokio::test]
    async fn span_logs_metadata_and_metrics() {
        let (span, collector) = build_test_span();
        span.log_metadata([("foo".to_string(), json!("bar"))].into_iter().collect())
            .await;
        span.log_usage(UsageMetrics {
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            reasoning_tokens: None,
            ..Default::default()
        })
        .await;
        span.finish().await.expect("finish");

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
        span.log_input(json!("data")).await;
        span.finish().await.expect("finish");

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
