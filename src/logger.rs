use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

use crate::error::{BraintrustError, Result};
use crate::span::SpanSubmitter;
use crate::types::{
    LogDestination, Logs3Request, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload,
    LOGS_API_VERSION,
};

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

    /// Submit a span payload for logging (fire-and-forget).
    ///
    /// Returns immediately after queuing. HTTP submission happens in the background.
    /// Errors are logged as warnings but not propagated to callers.
    pub async fn submit_payload(
        &self,
        token: String,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        let cmd = LogCommand::Submit(Box::new(SubmitCommand {
            token,
            payload,
            parent_info,
        }));
        self.inner
            .sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        Ok(())
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
}

async fn run_worker(base_url: Url, mut receiver: mpsc::Receiver<LogCommand>) {
    let mut state = WorkerState::new(base_url);
    while let Some(cmd) = receiver.recv().await {
        match cmd {
            LogCommand::Submit(submit) => {
                // Fire-and-forget: log errors but don't propagate
                if let Err(e) = state
                    .submit_payload(submit.token, submit.payload, submit.parent_info)
                    .await
                {
                    warn!(error = %e, "failed to submit span to Braintrust");
                }
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
            row_id,
            span_id,
            is_merge,
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

        // row_id and span_id come from payload - generated once at span creation, reused on every flush

        // Determine destination and span hierarchy based on parent info
        let (root_span_id, span_parents, destination) = match parent_info {
            None => {
                // No parent - use project_id if available, otherwise fail
                let dest = match project_id {
                    Some(pid) => LogDestination::project_logs(pid),
                    None => {
                        anyhow::bail!("no destination: either parent_info or project_name required")
                    }
                };
                (span_id.clone(), None, dest)
            }
            Some(ParentSpanInfo::Experiment { object_id }) => {
                (span_id.clone(), None, LogDestination::experiment(object_id))
            }
            Some(ParentSpanInfo::ProjectLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::project_logs(object_id),
            ),
            Some(ParentSpanInfo::ProjectName { project_name }) => {
                let proj_id = self
                    .ensure_project_id(&token, &org_id, org_name.as_deref(), &project_name)
                    .await?;
                (span_id.clone(), None, LogDestination::project_logs(proj_id))
            }
            Some(ParentSpanInfo::PlaygroundLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::playground_logs(object_id),
            ),
            Some(ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                span_id: parent_span_id,
                root_span_id: parent_root_span_id,
            }) => {
                let span_parents = Some(vec![parent_span_id]);
                let dest = match object_type {
                    SpanObjectType::Experiment => LogDestination::experiment(object_id),
                    SpanObjectType::ProjectLogs => LogDestination::project_logs(object_id),
                    SpanObjectType::PlaygroundLogs => LogDestination::playground_logs(object_id),
                };
                (parent_root_span_id, span_parents, dest)
            }
        };

        let row = Logs3Row {
            id: row_id,
            is_merge: if is_merge { Some(true) } else { None },
            span_id,
            root_span_id,
            span_parents,
            destination,
            org_id,
            org_name,
            input,
            output,
            metadata,
            metrics,
            span_attributes,
            created: Utc::now(),
        };

        let request = Logs3Request {
            rows: vec![row],
            api_version: LOGS_API_VERSION,
        };

        let json_bytes = serde_json::to_vec(&request)
            .map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))?;

        let response = self
            .client
            .post(logs_url)
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body(json_bytes)
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
    use crate::span::SpanLog;
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
            span.log(SpanLog {
                input: Some(Value::String("hello".into())),
                ..Default::default()
            })
            .await;
            span.flush().await.expect("flush");
            client.flush().await.expect("client flush");
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
        span.log(SpanLog {
            input: Some(Value::String("input".into())),
            ..Default::default()
        })
        .await;
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

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
