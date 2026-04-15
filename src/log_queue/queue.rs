use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::batching::batch_and_serialize_rows;
use super::config::LogQueueConfig;
use super::http::{fetch_version_info, save_payload_debug, send_batch_with_retry};
use super::merge::merge_row_into;
use super::row_key::RowKey;
use super::worker::LogCommand;
use crate::error::{BraintrustError, Result};
use crate::logger::LoginState;
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};
use arc_swap::ArcSwap;
use crossbeam::channel::{bounded, unbounded, Receiver, Sender, TrySendError};
use futures::future::join_all;
use indexmap::IndexMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex, OnceCell};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

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

/// A raw span payload queued for deferred preparation and flushing.
///
/// Fields are private — construction and destructuring happen only within this
/// module. `worker.rs` never touches the fields directly.
struct SubmitCommand {
    token: String,
    payload: SpanPayload,
    parent_info: Option<ParentSpanInfo>,
}

type CmdChannel = (Sender<SubmitCommand>, Receiver<SubmitCommand>);

/// Create a command channel (bounded or unbounded) based on the queue config.
fn make_cmd_channel(config: &LogQueueConfig) -> CmdChannel {
    if config.enforce_queue_size_limit() {
        bounded(config.queue_max_size())
    } else {
        unbounded()
    }
}

/// All state shared between `LogQueue` and the background worker.
///
/// Always accessed through `Arc<LogQueueCore>`. Fields no longer need their own `Arc`
/// wrappers — the outer `Arc` provides shared ownership and the field types provide
/// their own interior mutability where needed.
pub(super) struct LogQueueCore {
    /// Lock-free queue channel (sender, receiver) pair, atomically swappable.
    /// Holds unprocessed `SubmitCommand`s; preparation (project registration, row
    /// building) happens during `flush_internal`.
    channel: ArcSwap<CmdChannel>,
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    config: LogQueueConfig,
    login_state: LoginState,
    client: reqwest::Client,
    /// Cache of (org_id:project_name) → project_id to avoid redundant HTTP calls.
    project_cache: TokioMutex<IndexMap<String, String>>,
    /// Handle to the currently in-progress flush task, if any.
    /// Using JoinHandle allows checking completion status and awaiting the result.
    flush_handle: TokioMutex<Option<JoinHandle<std::result::Result<(), anyhow::Error>>>>,
    /// Lazily cached (max_request_size, can_use_overflow) from GET /version.
    /// Fetched once on first flush and reused thereafter.
    version_info: OnceCell<(usize, bool)>,
}

impl LogQueueCore {
    fn new(config: LogQueueConfig, login_state: LoginState, client: reqwest::Client) -> Arc<Self> {
        let (cmd_sender, cmd_receiver) = make_cmd_channel(&config);
        Arc::new(Self {
            channel: ArcSwap::from_pointee((cmd_sender, cmd_receiver)),
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            config,
            login_state,
            client,
            project_cache: TokioMutex::new(IndexMap::new()),
            flush_handle: TokioMutex::new(None),
            version_info: OnceCell::new(),
        })
    }

    /// Push a submit command directly to the lock-free queue.
    ///
    /// This is called by `LogQueue::submit` so that ALL drops (queue-full and
    /// channel-closed) are handled uniformly here — with drop count tracking,
    /// throttled warnings, and optional debug dumps.
    fn push(self: &Arc<Self>, cmd: SubmitCommand) {
        let channel = self.channel.load();
        match channel.0.try_send(cmd) {
            Ok(()) => {}
            Err(TrySendError::Full(cmd)) => {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
                self.log_drop_warning();
                self.dump_dropped_row_if_configured(cmd);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
                self.log_drop_warning();
            }
        }
    }

    /// Trigger a background flush if none is in progress.
    /// Non-blocking — spawns a tokio task if needed.
    /// No-op if no tokio runtime is available (e.g. in sync test contexts).
    async fn trigger_flush_background(self: &Arc<Self>) {
        let mut guard = self.flush_handle.lock().await;

        // Check if there's an existing flush in progress
        if let Some(handle) = guard.as_ref() {
            if !handle.is_finished() {
                // Flush already in progress
                return;
            }
        }

        // Consume the finished handle to surface any panic or error from the task.
        if let Some(handle) = guard.take() {
            if let Err(err) = handle.await {
                warn!("background flush task failed: {:?}", err);
            }
        }

        // No flush in progress, spawn a new one
        let Ok(runtime_handle) = tokio::runtime::Handle::try_current() else {
            // No runtime available
            return;
        };

        let core = self.clone();
        let join_handle = runtime_handle.spawn(async move {
            core.flush_internal().await;
            Ok(())
        });

        *guard = Some(join_handle);
    }

    /// Flush all rows in the lock-free queue and wait for completion.
    ///
    /// If a flush is already in progress, waits for it to complete and then
    /// flushes any remaining items (matches TypeScript SDK behavior).
    ///
    /// Called by the background worker in response to `LogCommand::Flush` /
    /// `LogCommand::TriggerFlush`. Does NOT drain the worker's mpsc channel —
    /// callers that need that should use `LogQueue::flush_all()`.
    pub(super) async fn flush(self: &Arc<Self>) -> std::result::Result<(), anyhow::Error> {
        loop {
            // Trigger a flush (no-op if one already running)
            self.trigger_flush_background().await;

            // Wait for the current flush to complete, if any
            let handle = {
                let mut guard = self.flush_handle.lock().await;
                guard.take()
            };

            if let Some(handle) = handle {
                // Wait for the flush task to complete and propagate any errors
                match handle.await {
                    Ok(Ok(())) => {
                        // Flush succeeded
                    }
                    Ok(Err(e)) => {
                        // Flush failed
                        return Err(e);
                    }
                    Err(e) => {
                        // Task panicked or was cancelled
                        return Err(anyhow::anyhow!("flush task failed: {}", e));
                    }
                }
            }

            // Check if queue is now empty
            if self.is_empty() {
                return Ok(());
            }

            // Queue is non-empty — items arrived while the flush was running.
            // Loop back to the top, which calls trigger_flush_background() again.
        }
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

    /// Internal flush implementation: drain → prepare → chunk → merge → batch → send.
    ///
    /// Commands are drained atomically. Each command is prepared (project
    /// registration, row building) before batching. Rows are then processed in
    /// sequential chunks of `min(batch_max_items, flush_chunk_size)`.
    /// Within each chunk, batches are sent concurrently.
    async fn flush_internal(self: &Arc<Self>) {
        let cmds = self.drain_all();
        if cmds.is_empty() {
            return;
        }

        let (api_key, api_url_str) = match (self.login_state.api_key(), self.login_state.api_url())
        {
            (Some(key), Some(url)) => (key, url),
            _ => {
                warn!("Cannot flush logs: not logged in");
                return;
            }
        };
        let org_name = self.login_state.org_name();

        let api_url = match Url::parse(&api_url_str) {
            Ok(url) => url,
            Err(e) => {
                warn!(error = %e, "Invalid API URL from login state");
                return;
            }
        };

        // Prepare rows from submit commands (project registration, destination resolution).
        let mut rows = Vec::with_capacity(cmds.len());
        for cmd in cmds {
            let SubmitCommand {
                token,
                payload,
                parent_info,
            } = cmd;
            match self.prepare_row(&token, payload, parent_info).await {
                Ok(row) => rows.push(row),
                Err(e) => {
                    warn!(error = %e, "failed to prepare span, dropping");
                }
            }
        }

        if rows.is_empty() {
            return;
        }

        let (max_request_size, can_use_overflow) = self
            .get_version_info(&api_url, &api_key, org_name.as_deref())
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
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "serialization task panicked, chunk dropped");
                vec![]
            });

            // Send all batches in this chunk concurrently (matches TypeScript SDK's Promise.all)
            let send_futures: Vec<_> = batches
                .into_iter()
                .map(|batch| {
                    send_batch_with_retry(
                        &self.client,
                        &api_url,
                        &api_key,
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
                }
            }
        }
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
                extra: HashMap::new(),
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
                        if let Some(object_id) = object_id {
                            LogDestination::project_logs(object_id)
                        } else {
                            let args = compute_object_metadata_args.as_ref().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "project-log parent span is missing compute_object_metadata_args"
                                )
                            })?;
                            if let Some(project_id) = args.get("project_id").and_then(Value::as_str)
                            {
                                LogDestination::project_logs(project_id.to_string())
                            } else {
                                let project_name = args
                                    .get("project_name")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| anyhow::anyhow!("missing project_name"))?;
                                let project_id = self
                                    .ensure_project_id(
                                        token,
                                        &org_id,
                                        org_name.as_deref(),
                                        project_name,
                                    )
                                    .await?;
                                LogDestination::project_logs(project_id)
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
            extra: HashMap::new(),
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
        if let Some(object_id) = span_components.object_id.as_ref() {
            return Ok(match span_components.object_type {
                SpanObjectType::Experiment => LogDestination::experiment(object_id.clone()),
                SpanObjectType::ProjectLogs => LogDestination::project_logs(object_id.clone()),
                SpanObjectType::PlaygroundLogs => {
                    LogDestination::playground_logs(object_id.clone())
                }
            });
        }

        match span_components.object_type {
            SpanObjectType::ProjectLogs => {
                let args = span_components
                    .compute_object_metadata_args
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing compute_object_metadata_args"))?;
                if let Some(project_id) = args.get("project_id").and_then(Value::as_str) {
                    return Ok(LogDestination::project_logs(project_id.to_string()));
                }
                let project_name = args
                    .get("project_name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("missing project_name"))?;
                let project_id = self
                    .ensure_project_id(token, org_id, org_name, project_name)
                    .await?;
                Ok(LogDestination::project_logs(project_id))
            }
            SpanObjectType::Experiment => anyhow::bail!("experiment span is missing object_id"),
            SpanObjectType::PlaygroundLogs => {
                anyhow::bail!("playground span is missing object_id")
            }
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

        let app_url_str = self
            .login_state
            .app_url()
            .ok_or_else(|| anyhow::anyhow!("cannot register project: not logged in"))?;
        let app_url =
            Url::parse(&app_url_str).map_err(|e| anyhow::anyhow!("invalid app URL: {e}"))?;
        let url = app_url
            .join("api/project/register")
            .map_err(|e| anyhow::anyhow!("invalid project register url: {e}"))?;

        let request = ProjectRegisterRequest {
            project_name,
            org_id: (!org_id.is_empty()).then_some(org_id),
            org_name,
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
            anyhow::bail!("register project failed: [{status}] {text}");
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

    /// Drain all commands from the queue with precise flush boundaries.
    ///
    /// Atomically swaps in a new channel using lock-free CAS, then drains the old channel.
    fn drain_all(&self) -> Vec<SubmitCommand> {
        let (new_sender, new_receiver) = make_cmd_channel(&self.config);
        let old_channel = self.channel.swap(Arc::new((new_sender, new_receiver)));
        let mut items = Vec::new();
        while let Ok(item) = old_channel.1.try_recv() {
            items.push(item);
        }
        items
    }

    /// Check if the queue is approximately empty.
    pub fn is_empty(&self) -> bool {
        self.channel.load().1.is_empty()
    }

    /// Get the approximate number of items in the queue (tests only).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.channel.load().1.len()
    }

    /// Get the total number of dropped items (tests only).
    #[cfg(test)]
    pub fn dropped_count(&self) -> usize {
        self.dropped_count.load(Ordering::Relaxed)
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
                    (SpanObjectType::ProjectLogs, None) => match span_components
                        .compute_object_metadata_args
                        .as_ref()
                        .and_then(|args| args.get("project_id"))
                        .and_then(Value::as_str)
                    {
                        Some(project_id) => LogDestination::project_logs(project_id.to_string()),
                        None => return,
                    },
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
                extra: HashMap::new(),
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

/// Lock-free log queue with batching and HTTP dispatch.
///
/// This is the complete log processing system:
/// - Owns the background worker for flush coordination
/// - Lock-free queue for accumulating submit commands (bounded or unbounded)
/// - Row preparation (project registration, destination resolution) deferred to flush time
/// - Automatic batching and serialization via spawn_blocking
/// - HTTP dispatch with retry logic (concurrent per-chunk batches)
/// - Atomic drain for precise flush boundaries
/// - Matches TypeScript SDK `activeFlush` pattern: only one flush runs at a time
/// - Lazy version fetch for dynamic maxRequestSize and overflow support
#[derive(Clone)]
pub struct LogQueue {
    /// All shared state, also held by the background worker via Arc.
    core: Arc<LogQueueCore>,
    /// Channel to the background worker (for flush commands only).
    worker_sender: mpsc::Sender<LogCommand>,
    /// Handle to the background worker task.
    #[allow(dead_code)]
    worker_handle: Arc<JoinHandle<()>>,
}

impl LogQueue {
    /// Create a new log queue that spawns and owns the background worker.
    ///
    /// The worker handles flush coordination; row preparation (project registration,
    /// destination resolution) happens inside `flush_internal` at flush time.
    pub fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: reqwest::Client,
        worker_queue_size: usize,
    ) -> Self {
        let core = LogQueueCore::new(config, login_state.clone(), client.clone());

        let (worker_sender, worker_receiver) = mpsc::channel(worker_queue_size.max(32));

        let worker_handle = Arc::new(tokio::spawn(super::worker::run_worker(
            worker_receiver,
            core.clone(),
        )));

        Self {
            core,
            worker_sender,
            worker_handle,
        }
    }

    /// Submit a span payload for processing.
    ///
    /// The command is pushed directly into the lock-free queue. All drops
    /// (queue-full and channel-closed) go through `LogQueueCore::push`, which
    /// handles drop counting, throttled warnings, and optional debug dumps
    /// uniformly. Row preparation (project registration, destination resolution)
    /// is deferred to flush time.
    pub fn submit(
        &self,
        token: impl Into<String>,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) {
        self.core.push(SubmitCommand {
            token: token.into(),
            payload,
            parent_info,
        });
        // Signal the worker to flush. try_send is non-blocking and best-effort:
        // if the channel is full the worker already has a flush pending.
        if !self.core.config.sync_flush() {
            let _ = self.worker_sender.try_send(LogCommand::TriggerFlush);
        }
    }

    /// Flush all pending data through the worker, waiting for completion.
    ///
    /// Sends a `Flush` command through the worker channel, which ensures any
    /// previously submitted rows (still in the worker's mpsc queue) are processed
    /// before the flush begins. This is the correct flush path for external callers.
    pub async fn flush_all(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.worker_sender
            .send(LogCommand::Flush(tx))
            .await
            .map_err(|_| BraintrustError::ChannelClosed)?;
        rx.await
            .map_err(|_| BraintrustError::ChannelClosed)?
            .map_err(|e| BraintrustError::Background(e.to_string()))
    }

    /// Trigger a non-blocking background flush via the worker.
    pub async fn trigger_flush_command(&self) -> Result<()> {
        self.worker_sender
            .send(LogCommand::TriggerFlush)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)
    }
}

impl Drop for LogQueue {
    fn drop(&mut self) {
        // Skip if not logged in (flush would return early anyway) or the lock-free
        // queue is visibly empty.
        //
        // Note: `is_empty()` only inspects the lock-free row queue, not the worker's
        // mpsc channel. Items submitted but not yet processed by the worker would be
        // missed by this guard. In practice the window is very small (a race between
        // Drop and the worker task), and when it does occur the worker's own "channel
        // closed" handler performs a final flush. Callers that need a hard guarantee
        // should call `flush_all()` before dropping.
        if !self.core.login_state.is_logged_in() || self.core.is_empty() {
            return;
        }

        // Use flush_all() rather than core.flush() directly. Routing through the
        // worker ensures any Submit commands ahead of this Drop in the mpsc queue are
        // processed before the flush begins.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(async {
                    let _ = self.flush_all().await;
                });
            });
        } else {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let _ = self.flush_all().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_queue::config::LogQueueConfig;

    fn make_test_cmd(id: &str) -> SubmitCommand {
        SubmitCommand {
            token: "test-token".to_string(),
            payload: SpanPayload {
                row_id: id.to_string(),
                span_id: id.to_string(),
                is_merge: false,
                span_components: None,
                org_id: "org".to_string(),
                org_name: Some("test-org".to_string()),
                project_name: None,
                input: None,
                output: None,
                expected: None,
                error: None,
                scores: None,
                metadata: None,
                metrics: None,
                tags: None,
                context: None,
                span_attributes: None,
            },
            parent_info: Some(ParentSpanInfo::Experiment {
                object_id: "exp-test".to_string(),
            }),
        }
    }

    fn make_queue(max_size: usize) -> LogQueue {
        // No login state set - prevents background flushes from making real HTTP calls
        // which would hang tests waiting for connections to non-existent servers.
        let config = LogQueueConfig::builder()
            .queue_max_size(max_size)
            .enforce_queue_size_limit(true) // Enable bounding so drop tests work correctly
            .sync_flush(true) // Disable auto-flush to avoid background HTTP calls
            .build();
        let login_state = LoginState::new();
        LogQueue::new(config, login_state, reqwest::Client::new(), 256)
    }

    #[tokio::test]
    async fn test_push_queues_item() {
        let queue = make_queue(10);
        assert!(queue.core.is_empty());
        queue.core.push(make_test_cmd("1"));
        assert_eq!(queue.core.len(), 1);
    }

    #[tokio::test]
    async fn test_push_drops_when_full() {
        let queue = make_queue(2);
        queue.core.push(make_test_cmd("1"));
        queue.core.push(make_test_cmd("2"));
        assert_eq!(queue.core.len(), 2);

        // This push should be dropped since the queue is full
        queue.core.push(make_test_cmd("3"));
        assert_eq!(queue.core.len(), 2);
        // The throttled warning fires immediately on the first drop (last_drop_log_time
        // starts at 0, which is well beyond the throttle window), resetting the counter.
        assert_eq!(queue.core.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_drop_count_accumulates_within_throttle_window() {
        // With a very long logging period the counter accumulates across multiple drops
        // without resetting (the throttle hasn't expired yet after the first warning fires).
        let config = LogQueueConfig::builder()
            .queue_max_size(1)
            .enforce_queue_size_limit(true)
            .sync_flush(true)
            .build();
        let login_state = LoginState::new();
        let queue = LogQueue::new(config, login_state, reqwest::Client::new(), 256);

        queue.core.push(make_test_cmd("1")); // fills queue

        // First drop: warning fires (last_drop_log_time=0 is always past throttle),
        // resets counter to 0 and records timestamp.
        queue.core.push(make_test_cmd("2")); // dropped, count reset to 0 after warning
        assert_eq!(queue.core.dropped_count(), 0);

        // Subsequent drops within the same throttle window accumulate without reset.
        queue.core.push(make_test_cmd("3")); // dropped, counter now 1
        queue.core.push(make_test_cmd("4")); // dropped, counter now 2
        assert_eq!(queue.core.dropped_count(), 2);
    }

    #[tokio::test]
    async fn test_unbounded_queue_never_drops() {
        let config = LogQueueConfig::builder()
            .enforce_queue_size_limit(false)
            .sync_flush(true)
            .build();
        let login_state = LoginState::new();
        let queue = LogQueue::new(config, login_state, reqwest::Client::new(), 256);

        // Push many more items than the default queue_max_size
        for i in 0..20_000 {
            queue.core.push(make_test_cmd(&i.to_string()));
        }
        assert_eq!(queue.core.len(), 20_000);
        assert_eq!(queue.core.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_drain_all_returns_all_items() {
        let queue = make_queue(10);
        queue.core.push(make_test_cmd("1"));
        queue.core.push(make_test_cmd("2"));
        queue.core.push(make_test_cmd("3"));

        let drained = queue.core.drain_all();
        assert_eq!(drained.len(), 3);
        assert!(queue.core.is_empty());
    }

    #[tokio::test]
    async fn test_drain_all_atomic_boundary() {
        // Items pushed after drain starts should go to the new channel, not the drained batch
        let queue = make_queue(10);
        queue.core.push(make_test_cmd("1"));
        queue.core.push(make_test_cmd("2"));

        let drained = queue.core.drain_all();
        assert_eq!(drained.len(), 2);

        // Push after drain goes to the new channel
        queue.core.push(make_test_cmd("3"));
        assert_eq!(queue.core.len(), 1);
    }

    #[tokio::test]
    async fn test_push_no_auto_flush_in_sync_mode() {
        // make_queue uses sync_flush=true, so push() should not trigger background flush
        let queue = make_queue(10);

        queue.core.push(make_test_cmd("1"));
        // No flush should be in progress - in sync mode user calls flush() explicitly
        assert!(queue.core.flush_handle.try_lock().unwrap().is_none());
        assert_eq!(queue.core.len(), 1);
    }
}
