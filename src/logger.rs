use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use url::Url;
use uuid::Uuid;

use crate::error::{BraintrustError, Result};
use crate::span::SpanSubmitter;
use crate::types::{Logs3Request, Logs3Row, ParentSpanInfo, SpanPayload, LOGS_API_VERSION};

const DEFAULT_QUEUE_SIZE: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct BraintrustClientConfig {
    pub api_url: String,
    pub app_url: String,
    pub queue_size: usize,
}

impl BraintrustClientConfig {
    pub fn new(api_url: impl Into<String>) -> Self {
        let api_url = api_url.into();
        Self {
            api_url: api_url.clone(),
            app_url: api_url,
            queue_size: DEFAULT_QUEUE_SIZE,
        }
    }

    pub fn with_app_url(mut self, app_url: impl Into<String>) -> Self {
        self.app_url = app_url.into();
        self
    }
}

impl From<String> for BraintrustClientConfig {
    fn from(value: String) -> Self {
        BraintrustClientConfig::new(value)
    }
}

impl From<&str> for BraintrustClientConfig {
    fn from(value: &str) -> Self {
        BraintrustClientConfig::new(value.to_string())
    }
}

#[derive(Clone)]
pub struct BraintrustClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    sender: mpsc::Sender<LogCommand>,
    #[allow(dead_code)]
    worker: JoinHandle<()>,
}

impl BraintrustClient {
    pub fn new(config: impl Into<BraintrustClientConfig>) -> Result<Self> {
        let config = config.into();
        let base_url = Url::parse(&config.api_url)
            .map_err(|e| BraintrustError::InvalidConfig(e.to_string()))?;

        let (sender, receiver) = mpsc::channel(config.queue_size.max(32));
        let worker = tokio::spawn(run_worker(base_url, receiver));

        Ok(Self {
            inner: Arc::new(ClientInner { sender, worker }),
        })
    }

    pub fn span_builder(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> crate::span::SpanBuilder {
        let submitter: Arc<dyn crate::span::SpanSubmitter> = Arc::new(self.clone());
        crate::span::SpanBuilder::new(submitter, token, org_id)
    }

    pub async fn submit_payload(
        &self,
        token: String,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let cmd = LogCommand::Submit(Box::new(SubmitCommand {
            token,
            payload,
            parent_info,
            response: tx,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }

    pub async fn flush(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .sender
            .send(LogCommand::Flush(tx))
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }
}

#[async_trait]
impl SpanSubmitter for BraintrustClient {
    async fn submit(
        &self,
        token: String,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        self.submit_payload(token, payload, parent_info).await
    }
}

enum LogCommand {
    Submit(Box<SubmitCommand>),
    Flush(oneshot::Sender<std::result::Result<(), anyhow::Error>>),
}

struct SubmitCommand {
    token: String,
    payload: SpanPayload,
    parent_info: Option<ParentSpanInfo>,
    response: oneshot::Sender<std::result::Result<(), anyhow::Error>>,
}

async fn run_worker(base_url: Url, mut receiver: mpsc::Receiver<LogCommand>) {
    let mut state = WorkerState::new(base_url);
    while let Some(cmd) = receiver.recv().await {
        match cmd {
            LogCommand::Submit(submit) => {
                let result = state
                    .submit_payload(submit.token, submit.payload, submit.parent_info)
                    .await;
                let _ = submit.response.send(result);
            }
            LogCommand::Flush(response) => {
                let _ = response.send(Ok(()));
            }
        }
    }
}

struct WorkerState {
    base_url: Url,
    client: reqwest::Client,
    project_cache: HashMap<String, String>,
}

impl WorkerState {
    fn new(base_url: Url) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client");
        Self {
            base_url,
            client,
            project_cache: HashMap::new(),
        }
    }

    async fn submit_payload(
        &mut self,
        token: String,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> std::result::Result<(), anyhow::Error> {
        let SpanPayload {
            org_id,
            org_name,
            project_name,
            name: _,
            input,
            output,
            metadata,
            metrics,
            span_attributes,
        } = payload;

        let project_id = if let Some(ref project_name) = project_name {
            Some(
                self.ensure_project_id(&token, &org_id, org_name.as_deref(), project_name)
                    .await?,
            )
        } else {
            None
        };

        let logs_url = self
            .base_url
            .join("logs3")
            .map_err(|e| anyhow::anyhow!("invalid logs url: {e}"))?;

        let row_id = Uuid::new_v4().to_string();
        let new_span_id = Uuid::new_v4().to_string();

        let (span_id, root_span_id, span_parents, computed_project_id, experiment_id) =
            match parent_info {
                None => (
                    new_span_id.clone(),
                    new_span_id.clone(),
                    None,
                    project_id.clone(),
                    None,
                ),
                Some(ParentSpanInfo::Experiment { object_id }) => (
                    new_span_id.clone(),
                    new_span_id.clone(),
                    None,
                    None,
                    Some(object_id),
                ),
                Some(ParentSpanInfo::ProjectLogs { object_id }) => (
                    new_span_id.clone(),
                    new_span_id.clone(),
                    None,
                    Some(object_id),
                    None,
                ),
                Some(ParentSpanInfo::ProjectName { project_name }) => {
                    let proj_id = self
                        .ensure_project_id(&token, &org_id, org_name.as_deref(), &project_name)
                        .await?;
                    (
                        new_span_id.clone(),
                        new_span_id.clone(),
                        None,
                        Some(proj_id),
                        None,
                    )
                }
                Some(ParentSpanInfo::PlaygroundLogs { object_id: _ }) => {
                    (new_span_id.clone(), new_span_id.clone(), None, None, None)
                }
                Some(ParentSpanInfo::FullSpan {
                    object_type,
                    object_id,
                    span_id: parent_span_id,
                    root_span_id: parent_root_span_id,
                }) => {
                    let span_parents = vec![parent_span_id];
                    match object_type {
                        1 => (
                            new_span_id.clone(),
                            parent_root_span_id,
                            Some(span_parents),
                            None,
                            Some(object_id),
                        ),
                        2 => (
                            new_span_id.clone(),
                            parent_root_span_id,
                            Some(span_parents),
                            Some(object_id),
                            None,
                        ),
                        3 => (
                            new_span_id.clone(),
                            parent_root_span_id,
                            Some(span_parents),
                            None,
                            None,
                        ),
                        _ => (
                            new_span_id.clone(),
                            parent_root_span_id,
                            Some(span_parents),
                            Some(object_id),
                            None,
                        ),
                    }
                }
            };

        let final_project_id = computed_project_id.or(project_id);
        let log_id = if experiment_id.is_some() {
            None
        } else {
            Some("g".to_string())
        };

        let row = Logs3Row {
            id: row_id,
            span_id: span_id.clone(),
            root_span_id: root_span_id.clone(),
            span_parents,
            project_id: final_project_id.clone(),
            experiment_id,
            log_id,
            org_id: Some(org_id.clone()),
            org_name: org_name.clone(),
            input,
            output,
            metadata,
            metrics,
            span_attributes,
            created: Some(Utc::now()),
        };

        let request = Logs3Request {
            rows: vec![row],
            api_version: LOGS_API_VERSION,
        };

        let response = self
            .client
            .post(logs_url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable>".to_string());
            tracing::warn!("failed to submit span: [{status}] {body}");
        }

        Ok(())
    }

    async fn ensure_project_id(
        &mut self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        project_name: &str,
    ) -> std::result::Result<String, anyhow::Error> {
        let cache_key = format!("{org_id}:{project_name}");
        if let Some(project_id) = self.project_cache.get(&cache_key) {
            return Ok(project_id.clone());
        }

        let mut body = serde_json::Map::new();
        body.insert(
            "project_name".to_string(),
            serde_json::Value::String(project_name.to_string()),
        );
        if !org_id.is_empty() {
            body.insert(
                "org_id".to_string(),
                serde_json::Value::String(org_id.to_string()),
            );
        }
        if let Some(name) = org_name {
            body.insert(
                "org_name".to_string(),
                serde_json::Value::String(name.to_string()),
            );
        }

        let url = self
            .base_url
            .join("api/project/register")
            .map_err(|e| anyhow::anyhow!("invalid project register url: {e}"))?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("register project failed: [{status}] {text}");
        }

        let json = response.json::<serde_json::Value>().await?;
        let project_id = json
            .get("project")
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_str())
            .context("project registration missing project.id")?
            .to_string();

        self.project_cache.insert(cache_key, project_id.clone());
        Ok(project_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn rejects_invalid_base_url() {
        let result = BraintrustClient::new(BraintrustClientConfig::new("::not a url::"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn project_registration_is_cached() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "test-project-id" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let client =
            BraintrustClient::new(BraintrustClientConfig::new(server.uri())).expect("client");

        for _ in 0..2 {
            let span = client
                .span_builder("token", "org-id")
                .org_name("org-name")
                .project_name("demo-project")
                .build();
            span.log_input(Value::String("hello".into())).await;
            span.finish().await.expect("finish");
            client.flush().await.expect("flush");
        }

        let register_calls = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/api/project/register")
            .count();

        assert_eq!(register_calls, 1);
    }

    #[tokio::test]
    async fn logs_request_contains_span_rows() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let client =
            BraintrustClient::new(BraintrustClientConfig::new(server.uri())).expect("client");

        let span = client
            .span_builder("token", "org-id")
            .project_name("demo-project")
            .build();
        span.log_input(Value::String("input".into())).await;
        span.finish().await.expect("finish");
        client.flush().await.expect("flush");

        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        assert!(body.get("rows").is_some());
    }
}
