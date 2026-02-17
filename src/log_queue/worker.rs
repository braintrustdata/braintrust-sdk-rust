use anyhow::Context;
use chrono::Utc;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;
use url::Url;

use crate::logger::LoginState;
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};

use super::queue::LogQueue;

pub(super) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Configuration for the background worker.
pub(super) struct WorkerConfig {
    pub(super) app_url: Url,
}

/// Commands sent through the worker channel.
pub(super) enum LogCommand {
    Submit(Box<SubmitCommand>),
    Flush(oneshot::Sender<std::result::Result<(), anyhow::Error>>),
    TriggerFlush,
}

pub(super) struct SubmitCommand {
    pub(super) token: String,
    pub(super) payload: SpanPayload,
    pub(super) parent_info: Option<ParentSpanInfo>,
}

/// Request body for project registration.
#[derive(Serialize)]
struct ProjectRegisterRequest<'a> {
    project_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    org_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    org_name: Option<&'a str>,
}

/// Response from project registration.
#[derive(Deserialize)]
struct ProjectRegisterResponse {
    project: ProjectInfo,
}

/// Project info in registration response.
#[derive(Deserialize)]
struct ProjectInfo {
    id: String,
}

pub(super) async fn run_worker(
    login_state: LoginState,
    mut receiver: mpsc::Receiver<LogCommand>,
    config: WorkerConfig,
    queue: LogQueue,
) {
    let mut state = WorkerState::new(login_state, config, queue);

    loop {
        match receiver.recv().await {
            Some(LogCommand::Submit(cmd)) => {
                let SubmitCommand {
                    token,
                    payload,
                    parent_info,
                } = *cmd;
                if let Err(e) = state
                    .prepare_and_queue_row(&token, payload, parent_info)
                    .await
                {
                    warn!(error = %e, "failed to queue span");
                }
            }
            Some(LogCommand::Flush(response)) => {
                let result = state.flush_pending().await;
                let _ = response.send(result);
            }
            Some(LogCommand::TriggerFlush) => {
                if let Err(e) = state.flush_pending().await {
                    warn!(error = %e, "background flush failed");
                }
            }
            None => {
                // Channel closed - flush remaining and exit
                if let Err(e) = state.flush_pending().await {
                    warn!(error = %e, "final flush failed");
                }
                break;
            }
        }
    }
}

/// Internal worker state - implementation detail of LogQueue.
struct WorkerState {
    app_url: Url,
    client: reqwest::Client,
    project_cache: IndexMap<String, String>,
    queue: LogQueue,
}

impl WorkerState {
    fn new(_login_state: LoginState, config: WorkerConfig, queue: LogQueue) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client");

        Self {
            app_url: config.app_url,
            client,
            project_cache: IndexMap::new(),
            queue,
        }
    }

    /// Prepare a row from payload and push it to the lock-free queue.
    async fn prepare_and_queue_row(
        &mut self,
        token: &str,
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
            input,
            output,
            expected,
            error,
            scores,
            metadata,
            metrics,
            tags,
            context,
            span_attributes,
        } = payload;

        let project_id = if let Some(ref project_name) = project_name {
            Some(
                self.ensure_project_id(token, &org_id, org_name.as_deref(), project_name)
                    .await?,
            )
        } else {
            None
        };

        let (root_span_id, span_parents, destination) = match parent_info {
            None => {
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
                    .ensure_project_id(token, &org_id, org_name.as_deref(), &project_name)
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
            expected,
            error,
            scores,
            metadata,
            metrics,
            tags,
            context,
            span_attributes,
            created: Utc::now(),
        };

        self.queue.push(row);

        Ok(())
    }

    async fn flush_pending(&mut self) -> std::result::Result<(), anyhow::Error> {
        self.queue
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))
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

        let request = ProjectRegisterRequest {
            project_name,
            org_id: (!org_id.is_empty()).then_some(org_id),
            org_name,
        };

        let url = self
            .app_url
            .join("api/project/register")
            .map_err(|e| anyhow::anyhow!("invalid project register url: {e}"))?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("register project failed: [{status}] {text}");
        }

        let register_response: ProjectRegisterResponse = response
            .json()
            .await
            .context("failed to parse project registration response")?;

        self.project_cache
            .insert(cache_key, register_response.project.id.clone());
        Ok(register_response.project.id)
    }
}
