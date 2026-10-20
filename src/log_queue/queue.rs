use anyhow::Context;
use chrono::Utc;

use super::batching::batch_and_serialize_rows;
use super::config::LogQueueConfig;
use super::http::{fetch_version_info, save_payload_debug, send_batch_with_retry};
use super::merge::merge_row_into;
use super::row_key::RowKey;
use crate::error::{BraintrustError, Result};
use crate::internal::delivery::WriteService;
use crate::logger::LoginState;
use crate::span_components::{project_logs_identifier, ProjectLogsIdentifier};
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};
use crate::{
    Completion, DeliveryReceipt, FlushError, FlushReport, ShutdownError, ShutdownReport, WriteError,
};
use futures::future::join_all;
use indexmap::IndexMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as TokioMutex, OnceCell};
use tracing::warn;
use url::Url;

use crate::api::projects::{ProjectRegisterRequest, ProjectRegisterResponse};

/// A raw span payload queued for deferred preparation and flushing.
///
/// Fields are private — construction and destructuring happen only within this
/// module. `worker.rs` never touches the fields directly.
struct SubmitCommand {
    token: String,
    payload: SpanPayload,
    parent_info: Option<ParentSpanInfo>,
}

/// Preparation and protocol orchestration, independent of admission/lifecycle.
struct LogQueueCore {
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    config: LogQueueConfig,
    login_state: LoginState,
    client: reqwest::Client,
    api_url: Url,
    app_url: Url,
    project_cache: TokioMutex<IndexMap<String, String>>,
    version_info: OnceCell<(usize, bool)>,
}
impl LogQueueCore {
    fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: reqwest::Client,
        api_url: Url,
        app_url: Url,
    ) -> Arc<Self> {
        Arc::new(Self {
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            config,
            login_state,
            client,
            api_url,
            app_url,
            project_cache: TokioMutex::new(IndexMap::new()),
            version_info: OnceCell::new(),
        })
    }

    /// Get the cached (max_request_size, can_use_overflow) from GET /version.
    /// Fetches lazily on first call and caches the result.
    async fn get_version_info(
        &self,
        api_url: &Url,
        token: &str,
        org_name: Option<&str>,
    ) -> (usize, bool) {
        *self
            .version_info
            .get_or_init(|| async {
                fetch_version_info(&self.client, api_url, token, org_name).await
            })
            .await
    }

    /// Process one detached logical batch: prepare → merge → encode → upload.
    ///
    /// The write service owns membership and the fixed cutoff. Each command is
    /// prepared (project registration, row building) before batching. Rows are then processed in
    /// sequential chunks of `min(batch_max_items, flush_chunk_size)`.
    /// Within each chunk, batches are sent concurrently.
    async fn process(
        self: &Arc<Self>,
        cmds: Vec<SubmitCommand>,
    ) -> std::result::Result<(), anyhow::Error> {
        if cmds.is_empty() {
            return Ok(());
        }
        let mut first_error = None;

        // Resolve the data-plane URL. Prefer the shared login state (covers the
        // logged-in path, including an org-specific api_url returned by login) and
        // fall back to the client-configured URL so per-request-credential callers
        // work without ever populating the shared login state.
        let api_url_str = self
            .login_state
            .api_url()
            .unwrap_or_else(|| self.api_url.to_string());
        let api_url = match Url::parse(&api_url_str) {
            Ok(url) => url,
            Err(e) => {
                warn!(error = %e, "Invalid API URL");
                return Err(e.into());
            }
        };

        // Prepare rows and bucket them by their upload credentials. The batch
        // "destination" is (token, org_id): rows uploaded with different tokens or
        // orgs must never be merged or batched into the same request under one
        // token. `org_name` is carried alongside for the `x-bt-org-name` header.
        type CredKey = (String, String);
        let mut groups: IndexMap<CredKey, (Option<String>, Vec<Logs3Row>)> = IndexMap::new();
        for cmd in cmds {
            let SubmitCommand {
                token,
                payload,
                parent_info,
            } = cmd;
            match self.prepare_row(&token, payload, parent_info).await {
                Ok(row) => {
                    let key = (token, row.org_id.clone());
                    let org_name = row.org_name.clone().or_else(|| self.login_state.org_name());
                    groups
                        .entry(key)
                        .or_insert_with(|| (org_name, Vec::new()))
                        .1
                        .push(row);
                }
                Err(e) => {
                    warn!(error = %e, "failed to prepare span, dropping");
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        if groups.is_empty() {
            return first_error.map_or(Ok(()), Err);
        }

        // Version info reflects a server-wide limit, so it is fetched once and
        // reused across all credential groups (using any group's credentials).
        let (probe_token, probe_org) = {
            let ((token, _org_id), (org_name, _rows)) = groups.get_index(0).unwrap();
            (token.clone(), org_name.clone())
        };
        let (max_request_size, can_use_overflow) = self
            .get_version_info(&api_url, &probe_token, probe_org.as_deref())
            .await;

        // Effective batch byte limit: the smaller of the locally configured limit and half the
        // server-advertised max request size. This mirrors the TypeScript SDK which uses
        // `maxRequestSize / 2` as the batching limit, where maxRequestSize comes from /version.
        let effective_batch_bytes = self.config.batch_max_bytes().min(max_request_size / 2);

        // Chunk size matches TypeScript SDK: max(1, min(batchMaxItems, flushChunkSize))
        let chunk_size = std::cmp::max(
            1,
            std::cmp::min(
                self.config.batch_max_items(),
                self.config.flush_chunk_size(),
            ),
        );

        for ((token, _org_id), (org_name, rows)) in groups {
            // Process chunks sequentially (matches TypeScript SDK)
            let chunks: Vec<Vec<Logs3Row>> = rows.chunks(chunk_size).map(<[_]>::to_vec).collect();

            for chunk in chunks {
                let config = self.config.clone();
                let batches = tokio::task::spawn_blocking(move || {
                    let mut merged: IndexMap<RowKey, Logs3Row> = IndexMap::new();
                    for row in chunk {
                        let key = RowKey::from_row(&row);
                        if let Some(existing) = merged.get_mut(&key) {
                            if row.is_merge.unwrap_or(false) {
                                merge_row_into(existing, row);
                            } else {
                                *existing = row;
                            }
                        } else {
                            merged.insert(key, row);
                        }
                    }

                    let rows: Vec<Logs3Row> = merged.into_values().collect();
                    batch_and_serialize_rows(rows, &config, effective_batch_bytes)
                })
                .await;
                let batches = match batches {
                    Ok(Ok(batches)) => batches,
                    Ok(Err(error)) => {
                        warn!(%error, "failed to serialize batch");
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                        vec![]
                    }
                    Err(error) => {
                        warn!(%error, "serialization task panicked");
                        if first_error.is_none() {
                            first_error = Some(error.into());
                        }
                        vec![]
                    }
                };

                // Send all batches in this chunk concurrently (matches TypeScript SDK's Promise.all).
                // Each request uses THIS group's per-request token and org.
                let send_futures: Vec<_> = batches
                    .into_iter()
                    .map(|batch| {
                        send_batch_with_retry(
                            &self.client,
                            &api_url,
                            &token,
                            org_name.as_deref(),
                            batch,
                            &self.config,
                            max_request_size,
                            can_use_overflow,
                        )
                    })
                    .collect();

                let results = join_all(send_futures).await;
                for result in results {
                    if let Err(e) = result {
                        warn!(error = %e, "batch send failed");
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Prepare a `Logs3Row` from a submit command's token, payload, and parent info.
    ///
    /// Resolves the destination (including calling the project registration API
    /// if needed) and builds the complete row for the flush pipeline.
    async fn prepare_row(
        &self,
        token: &str,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> std::result::Result<Logs3Row, anyhow::Error> {
        let SpanPayload {
            row_id,
            span_id,
            is_merge,
            span_components,
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
            extra,
        } = payload;

        if let Some(span_components) = span_components {
            let destination = self
                .destination_from_span_components(
                    token,
                    &org_id,
                    org_name.as_deref(),
                    &span_components,
                )
                .await?;
            let root_span_id = span_components
                .root_span_id
                .clone()
                .unwrap_or_else(|| span_id.clone());

            return Ok(Logs3Row {
                id: row_id,
                is_merge: if is_merge { Some(true) } else { None },
                merge_paths: None,
                span_id,
                root_span_id,
                span_parents: span_components.span_parents.clone(),
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
                extra,
                created: Utc::now(),
                xact_id: None,
                object_delete: None,
                audit_source: Some("api".to_string()),
            });
        }

        let project_id = if let Some(ref pn) = project_name {
            Some(
                self.ensure_project_id(token, &org_id, org_name.as_deref(), pn)
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
            Some(ParentSpanInfo::Dataset { object_id }) => {
                (span_id.clone(), None, LogDestination::dataset(object_id))
            }
            Some(ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                compute_object_metadata_args,
                span_id: parent_span_id,
                root_span_id: parent_root_span_id,
                span_parents: _,
                propagated_event: _,
            }) => {
                let span_parents = Some(vec![parent_span_id]);
                let dest = match object_type {
                    SpanObjectType::Experiment => {
                        LogDestination::experiment(object_id.ok_or_else(|| {
                            anyhow::anyhow!("experiment parent span is missing object_id")
                        })?)
                    }
                    SpanObjectType::ProjectLogs => {
                        match project_logs_identifier(
                            object_id.as_deref(),
                            compute_object_metadata_args.as_ref(),
                        ) {
                            Some(ProjectLogsIdentifier::ObjectId(object_id)) => {
                                LogDestination::project_logs(object_id)
                            }
                            Some(ProjectLogsIdentifier::ProjectName(project_name)) => {
                                let project_id = self
                                    .ensure_project_id(
                                        token,
                                        &org_id,
                                        org_name.as_deref(),
                                        &project_name,
                                    )
                                    .await?;
                                LogDestination::project_logs(project_id)
                            }
                            None => {
                                anyhow::bail!(
                                    "project-log parent span is missing object_id, project_id, or project_name"
                                )
                            }
                        }
                    }
                    SpanObjectType::PlaygroundLogs => {
                        LogDestination::playground_logs(object_id.ok_or_else(|| {
                            anyhow::anyhow!("playground parent span is missing object_id")
                        })?)
                    }
                };
                (parent_root_span_id, span_parents, dest)
            }
        };

        Ok(Logs3Row {
            id: row_id,
            is_merge: if is_merge { Some(true) } else { None },
            merge_paths: None,
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
            extra,
            created: Utc::now(),
            xact_id: None,
            object_delete: None,
            audit_source: Some("api".to_string()),
        })
    }

    async fn destination_from_span_components(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        span_components: &crate::span_components::SpanComponents,
    ) -> std::result::Result<LogDestination, anyhow::Error> {
        match span_components.object_type {
            SpanObjectType::Experiment => span_components
                .object_id
                .as_ref()
                .map(|object_id| LogDestination::experiment(object_id.clone()))
                .ok_or_else(|| anyhow::anyhow!("experiment span is missing object_id")),
            SpanObjectType::ProjectLogs => match span_components.project_logs_identifier() {
                Some(ProjectLogsIdentifier::ObjectId(object_id)) => {
                    Ok(LogDestination::project_logs(object_id))
                }
                Some(ProjectLogsIdentifier::ProjectName(project_name)) => {
                    let project_id = self
                        .ensure_project_id(token, org_id, org_name, &project_name)
                        .await?;
                    Ok(LogDestination::project_logs(project_id))
                }
                None => anyhow::bail!(
                    "project-log span is missing object_id, project_id, or project_name"
                ),
            },
            SpanObjectType::PlaygroundLogs => span_components
                .object_id
                .as_ref()
                .map(|object_id| LogDestination::playground_logs(object_id.clone()))
                .ok_or_else(|| anyhow::anyhow!("playground span is missing object_id")),
        }
    }

    /// Ensure a project ID is available for the given project name, registering
    /// it via the API if it is not yet cached.
    ///
    /// Uses `login_state.app_url()` for the registration endpoint so the URL
    /// doesn't need to be passed separately.
    async fn ensure_project_id(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        project_name: &str,
    ) -> std::result::Result<String, anyhow::Error> {
        let cache_key = format!("{org_id}:{project_name}");
        {
            let cache = self.project_cache.lock().await;
            if let Some(project_id) = cache.get(&cache_key) {
                return Ok(project_id.clone());
            }
        }

        // Prefer the login-state control-plane URL (logged-in path), falling back
        // to the client-configured URL for per-request-credential callers.
        let app_url = match self.login_state.app_url() {
            Some(app_url_str) => {
                Url::parse(&app_url_str).map_err(|e| anyhow::anyhow!("invalid app URL: {e}"))?
            }
            None => self.app_url.clone(),
        };
        let url = app_url
            .join("api/project/register")
            .map_err(|e| anyhow::anyhow!("invalid project register url: {e}"))?;

        let request = ProjectRegisterRequest {
            project_name: project_name.to_owned(),
            org_id: (!org_id.is_empty()).then(|| org_id.to_owned()),
            org_name: org_name.map(str::to_owned),
        };

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
            return Err(BraintrustError::Api {
                status: status.as_u16(),
                message: text,
            }
            .into());
        }

        let register_response: ProjectRegisterResponse = response
            .json()
            .await
            .context("failed to parse project registration response")?;

        {
            let mut cache = self.project_cache.lock().await;
            cache.insert(cache_key, register_response.project.id.clone());
        }
        Ok(register_response.project.id)
    }

    /// Log a drop warning with throttling.
    ///
    /// Reports the number of items dropped since the last warning was emitted,
    /// then resets the counter. Throttled to at most once per `queue_drop_logging_period`.
    fn log_drop_warning(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let last_log = self.last_drop_log_time.load(Ordering::Relaxed);
        let throttle_secs = self.config.queue_drop_logging_period().as_secs();

        if now - last_log >= throttle_secs
            && self
                .last_drop_log_time
                .compare_exchange(last_log, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let count = self.dropped_count.swap(0, Ordering::Relaxed);
            if count > 0 {
                warn!(
                    "Log queue full (size: {}), dropped {} items since last warning. \
                     Consider increasing BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE.",
                    self.config.queue_max_size(),
                    count
                );
            }
        }
    }

    /// If debug directories are configured, serialize the dropped command's payload
    /// and write it to disk. Spawns a background task — best-effort, non-blocking.
    fn dump_dropped_row_if_configured(&self, cmd: SubmitCommand) {
        let all_dir = self.config.all_publish_payloads_dir();
        let failed_dir = self.config.failed_publish_payloads_dir();
        let dirs: Vec<_> = [all_dir, failed_dir].into_iter().flatten().collect();
        if dirs.is_empty() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let SubmitCommand {
                token: _,
                payload,
                parent_info,
            } = cmd;

            let span_components = payload.span_components.clone();

            let span_id = payload.span_id.clone();
            let destination = match span_components.as_ref() {
                Some(span_components) => match (
                    span_components.object_type,
                    span_components.object_id.as_ref(),
                ) {
                    (SpanObjectType::Experiment, Some(object_id)) => {
                        LogDestination::experiment(object_id.clone())
                    }
                    (SpanObjectType::ProjectLogs, Some(object_id)) => {
                        LogDestination::project_logs(object_id.clone())
                    }
                    (SpanObjectType::PlaygroundLogs, Some(object_id)) => {
                        LogDestination::playground_logs(object_id.clone())
                    }
                    (SpanObjectType::ProjectLogs, None) => {
                        match span_components.project_logs_identifier() {
                            Some(ProjectLogsIdentifier::ObjectId(project_id)) => {
                                LogDestination::project_logs(project_id)
                            }
                            _ => return,
                        }
                    }
                    (SpanObjectType::Experiment, None) => return,
                    (SpanObjectType::PlaygroundLogs, None) => return,
                },
                None => match parent_info.as_ref() {
                    Some(ParentSpanInfo::Experiment { object_id }) => {
                        LogDestination::experiment(object_id.clone())
                    }
                    Some(ParentSpanInfo::ProjectLogs { object_id }) => {
                        LogDestination::project_logs(object_id.clone())
                    }
                    Some(ParentSpanInfo::ProjectName { .. }) => return,
                    Some(ParentSpanInfo::PlaygroundLogs { object_id }) => {
                        LogDestination::playground_logs(object_id.clone())
                    }
                    Some(ParentSpanInfo::Dataset { object_id }) => {
                        LogDestination::dataset(object_id.clone())
                    }
                    Some(ParentSpanInfo::FullSpan {
                        object_type,
                        object_id,
                        ..
                    }) => match object_type {
                        SpanObjectType::Experiment => match object_id.clone() {
                            Some(object_id) => LogDestination::experiment(object_id),
                            None => return,
                        },
                        SpanObjectType::ProjectLogs => match object_id.clone() {
                            Some(object_id) => LogDestination::project_logs(object_id),
                            None => return,
                        },
                        SpanObjectType::PlaygroundLogs => match object_id.clone() {
                            Some(object_id) => LogDestination::playground_logs(object_id),
                            None => return,
                        },
                    },
                    None => return,
                },
            };

            let root_span_id = span_components
                .as_ref()
                .and_then(|components| components.root_span_id.clone())
                .unwrap_or_else(|| span_id.clone());
            let row = Logs3Row {
                id: payload.row_id,
                span_id: span_id.clone(),
                is_merge: if payload.is_merge { Some(true) } else { None },
                merge_paths: None,
                root_span_id,
                span_parents: span_components
                    .as_ref()
                    .and_then(|components| components.span_parents.clone()),
                destination,
                org_id: payload.org_id,
                org_name: payload.org_name,
                input: payload.input,
                output: payload.output,
                expected: payload.expected,
                error: payload.error,
                scores: payload.scores,
                metadata: payload.metadata,
                metrics: payload.metrics,
                tags: payload.tags,
                context: payload.context,
                span_attributes: payload.span_attributes,
                extra: payload.extra,
                created: Utc::now(),
                xact_id: None,
                object_delete: None,
                audit_source: Some("api".to_string()),
            };

            let payload_bytes = match serde_json::to_vec(&serde_json::json!({
                "rows": [row],
                "api_version": crate::types::LOGS_API_VERSION,
            })) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "failed to serialize dropped row for debug dump");
                    return;
                }
            };
            for dir in dirs {
                if let Err(e) = save_payload_debug(&payload_bytes, Some(dir), "dropped") {
                    warn!(error = %e, "failed to write dropped row to debug dir");
                }
            }
        });
    }
}

/// Compatibility adapter connecting span preparation to the bounded write service.
#[derive(Clone)]
pub struct LogQueue {
    core: Arc<LogQueueCore>,
    writes: WriteService<SubmitCommand>,
}
impl LogQueue {
    pub fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: reqwest::Client,
        _worker_queue_size: usize,
        api_url: Url,
        app_url: Url,
    ) -> Self {
        let capacity = config.queue_max_size();
        let core = LogQueueCore::new(config, login_state, client, api_url, app_url);
        let processor = core.clone();
        let writes = WriteService::new(
            capacity,
            tokio::runtime::Handle::current(),
            move |commands| {
                let processor = processor.clone();
                Box::pin(async move { processor.process(commands).await })
            },
        );
        Self { core, writes }
    }

    pub fn submit(
        &self,
        token: impl Into<String>,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Completion<DeliveryReceipt, WriteError> {
        let command = SubmitCommand {
            token: token.into(),
            payload,
            parent_info,
        };
        self.writes
            .enqueue_with_rejection(command, |command, error| {
                self.core.dropped_count.fetch_add(1, Ordering::Relaxed);
                if matches!(error, crate::EnqueueError::Full) {
                    self.core.log_drop_warning();
                } else {
                    warn!(%error, "span submission rejected");
                }
                self.core.dump_dropped_row_if_configured(command);
            })
    }

    pub fn flush(&self) -> Completion<FlushReport, FlushError> {
        self.writes.flush()
    }
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> Completion<ShutdownReport, ShutdownError> {
        self.writes.shutdown(timeout)
    }
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }
    pub async fn trigger_flush_command(&self) -> Result<()> {
        let token = self.flush();
        if let Err(error) = &*token {
            return Err(error.clone().into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn payload(id: &str, merge: bool) -> SpanPayload {
        SpanPayload {
            row_id: id.into(),
            span_id: id.into(),
            is_merge: merge,
            span_components: None,
            org_id: "org".into(),
            org_name: Some("tenant".into()),
            project_name: None,
            input: Some(json!({"value": if merge { 2 } else { 1 }})),
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            extra: std::collections::HashMap::new(),
        }
    }
    fn queue(server: &MockServer) -> LogQueue {
        LogQueue::new(
            LogQueueConfig::builder().num_retries(0).build(),
            LoginState::new(),
            reqwest::Client::new(),
            32,
            Url::parse(&server.uri()).unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
    }
    fn parent() -> Option<ParentSpanInfo> {
        Some(ParentSpanInfo::Experiment {
            object_id: "experiment".into(),
        })
    }

    #[tokio::test]
    async fn late_route_failure_fails_merged_members_and_successful_siblings_then_retires() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&server)
            .await;
        let queue = queue(&server);
        let first = queue.submit("good", payload("merged", false), parent());
        let update = queue.submit("good", payload("merged", true), parent());
        let last = queue.submit("bad", payload("other", false), parent());
        let Err(FlushError::Delivery(failure)) = queue.flush().await else {
            panic!("expected failed flush");
        };
        assert_eq!(
            failure.failed, 3,
            "count accepted operations before merging"
        );
        for token in [first, update, last] {
            let Err(WriteError::Delivery(error)) = token.await else {
                panic!("expected batch error");
            };
            assert!(Arc::ptr_eq(&error, &failure.cause));
        }
        assert!(queue.is_empty());
        assert_eq!(queue.flush().await.unwrap().delivered, 0);
        let requests = server.received_requests().await.unwrap();
        let uploads: Vec<_> = requests
            .iter()
            .filter(|req| req.url.path() == "/logs3")
            .collect();
        assert_eq!(uploads.len(), 2);
        let first_body: Value = serde_json::from_slice(&uploads[0].body).unwrap();
        assert_eq!(first_body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(first_body["rows"][0]["input"]["value"], 2);
        let fresh = queue.submit("good", payload("fresh", false), parent());
        assert_eq!(queue.flush().await.unwrap().delivered, 1);
        fresh.await.unwrap();
        queue
            .shutdown(std::time::Duration::from_secs(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_split_sibling_fails_every_member_despite_an_earlier_committed_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(|request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let rows = body["rows"].as_array().unwrap();
                ResponseTemplate::new(if rows.len() > 1 {
                    413
                } else if rows[0]["id"] == "late-failure" {
                    400
                } else {
                    200
                })
            })
            .expect(3)
            .mount(&server)
            .await;
        let queue = queue(&server);
        let first = queue.submit("token", payload("committed", false), parent());
        let last = queue.submit("token", payload("late-failure", false), parent());
        let Err(FlushError::Delivery(failure)) = queue.flush().await else {
            panic!("expected failure");
        };
        assert_eq!(failure.failed, 2);
        for token in [first, last] {
            assert!(matches!(token.await, Err(WriteError::Delivery(_))));
        }
        assert!(queue.is_empty());
        queue.flush().await.unwrap();
        queue
            .shutdown(std::time::Duration::from_secs(1))
            .await
            .unwrap();
    }
}
