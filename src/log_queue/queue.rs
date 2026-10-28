use chrono::Utc;

use super::batching::batch_and_serialize_rows;
use super::config::LogQueueConfig;
use super::http::{fetch_version_info, save_payload_debug, send_batch_with_retry};
use super::merge::merge_row_into;
use super::row_key::RowKey;
use crate::error::{BraintrustError, Result};
use crate::internal::delivery::{WriteLease, WriteService};
use crate::internal::execution::ExecutionBinding;
use crate::internal::ports::{DeliveryControl, PreparedSpan, SpanOperation, SpanSubmitter};
use crate::internal::session::CredentialScope;
use crate::internal::shared_outcome::WriteOutcome;
use crate::span_components::{project_logs_identifier, ProjectLogsIdentifier};
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};
use crate::{
    Completion, DeliveryReceipt, FlushError, FlushReport, ShutdownError, ShutdownReport, WriteError,
};
use futures::future::{join_all, BoxFuture};
use indexmap::IndexMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as TokioMutex;
use tracing::warn;

use crate::api::projects::ProjectRegisterRequest;

// Bound successful lookup caches too, so one shared client cannot retain an
// unlimited number of per-tenant credentials. Eviction only repeats a lookup.
const LOOKUP_CACHE_CAPACITY: usize = 1024;
fn cache_insert<K: Eq + std::hash::Hash, V>(cache: &mut IndexMap<K, V>, key: K, value: V) {
    if cache.len() >= LOOKUP_CACHE_CAPACITY && !cache.contains_key(&key) {
        cache.shift_remove_index(0);
    }
    cache.insert(key, value);
}

/// A raw span payload queued for deferred preparation and flushing.
///
/// Fields are private — construction and destructuring happen only within this
/// module. `worker.rs` never touches the fields directly.
enum SubmitCommand {
    Ready(Box<PreparedSpan>),
    Deferred {
        row_id: String,
        prepare: BoxFuture<'static, Result<PreparedSpan>>,
    },
}

/// Preparation and protocol orchestration, independent of admission/lifecycle.
struct LogQueueCore {
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    config: LogQueueConfig,
    client: reqwest::Client,
    execution: ExecutionBinding,
    project_cache: TokioMutex<IndexMap<(CredentialScope, String), String>>,
    version_info: TokioMutex<IndexMap<CredentialScope, (usize, bool)>>,
}
impl LogQueueCore {
    fn new(
        config: LogQueueConfig,
        client: reqwest::Client,
        execution: ExecutionBinding,
    ) -> Arc<Self> {
        Arc::new(Self {
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            config,
            client,
            execution,
            project_cache: TokioMutex::new(IndexMap::new()),
            version_info: TokioMutex::new(IndexMap::new()),
        })
    }

    /// Capabilities are cached within the exact authentication and routing scope.
    async fn get_version_info(&self, scope: &CredentialScope) -> (usize, bool) {
        if let Some(info) = self.version_info.lock().await.get(scope).copied() {
            return info;
        }
        let info = fetch_version_info(
            &self.client,
            scope.api_url(),
            scope.token(),
            scope.org_name(),
        )
        .await;
        cache_insert(&mut *self.version_info.lock().await, scope.clone(), info);
        info
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

        // No live session lookup: registration, merging and upload share the
        // exact scope captured by each producer when its handle was created.
        let mut groups: IndexMap<CredentialScope, Vec<Logs3Row>> = IndexMap::new();
        for cmd in cmds {
            let prepared = async {
                let PreparedSpan {
                    scope,
                    payload,
                    parent_info,
                } = match cmd {
                    SubmitCommand::Ready(span) => *span,
                    SubmitCommand::Deferred { prepare, .. } => prepare.await?,
                };
                let row = self.prepare_row(&scope, payload, parent_info).await?;
                Ok::<_, anyhow::Error>((scope, row))
            }
            .await;
            match prepared {
                Ok((scope, row)) => {
                    groups.entry(scope).or_default().push(row);
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

        // Chunk size matches TypeScript SDK: max(1, min(batchMaxItems, flushChunkSize))
        let chunk_size = std::cmp::max(
            1,
            std::cmp::min(
                self.config.batch_max_items(),
                self.config.flush_chunk_size(),
            ),
        );

        for (scope, rows) in groups {
            let (max_request_size, can_use_overflow) = self.get_version_info(&scope).await;
            // Preserve the queue policy of half the advertised request limit.
            let effective_batch_bytes = self.config.batch_max_bytes().min(max_request_size / 2);
            // Process chunks sequentially (matches TypeScript SDK)
            let chunks: Vec<Vec<Logs3Row>> = rows.chunks(chunk_size).map(<[_]>::to_vec).collect();

            for chunk in chunks {
                let config = self.config.clone();
                let batches = self
                    .execution
                    .spawn_blocking(move || {
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
                            scope.api_url(),
                            scope.token(),
                            scope.org_name(),
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
        scope: &CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> std::result::Result<Logs3Row, anyhow::Error> {
        let SpanPayload {
            row_id,
            span_id,
            is_merge,
            span_components,
            org_id: _,
            org_name: _,
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

        let org_id = scope.org_id().to_owned();
        let org_name = scope.org_name().map(str::to_owned);

        if let Some(span_components) = span_components {
            let destination = self
                .destination_from_span_components(scope, &span_components)
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
            Some(self.ensure_project_id(scope, pn).await?)
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
                let proj_id = self.ensure_project_id(scope, &project_name).await?;
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
                                let project_id =
                                    self.ensure_project_id(scope, &project_name).await?;
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
        scope: &CredentialScope,
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
                    let project_id = self.ensure_project_id(scope, &project_name).await?;
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

    /// Resolve project names only within the producer's immutable request scope.
    async fn ensure_project_id(
        &self,
        scope: &CredentialScope,
        project_name: &str,
    ) -> std::result::Result<String, anyhow::Error> {
        let cache_key = (scope.clone(), project_name.to_owned());
        if let Some(id) = self.project_cache.lock().await.get(&cache_key).cloned() {
            return Ok(id);
        }
        let response = scope
            .api(&self.client)?
            .projects()
            .register(ProjectRegisterRequest {
                project_name: project_name.to_owned(),
                org_id: (!scope.org_id().is_empty()).then(|| scope.org_id().to_owned()),
                org_name: scope.org_name().map(str::to_owned),
            })
            .await
            .map_err(BraintrustError::from)?;
        cache_insert(
            &mut *self.project_cache.lock().await,
            cache_key,
            response.project.id.clone(),
        );
        Ok(response.project.id)
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
        self.execution.spawn(async move {
            let PreparedSpan {
                scope: _,
                payload,
                parent_info,
            } = match cmd {
                SubmitCommand::Ready(span) => *span,
                SubmitCommand::Deferred { row_id, .. } => {
                    // Rejection must not initiate login or registration merely
                    // to serialize a concrete wire row for a debug dump.
                    warn!(%row_id, "cannot dump rejected row with unresolved scope or destination");
                    return;
                }
            };
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
    writes: Arc<Mutex<QueueState>>,
    lease: Option<WriteLease<SubmitCommand>>,
}
enum QueueState {
    Dormant,
    Running(WriteService<SubmitCommand>),
    Closed,
}
impl LogQueue {
    pub fn new(
        config: LogQueueConfig,
        client: reqwest::Client,
        _worker_queue_size: usize,
        execution: ExecutionBinding,
    ) -> Self {
        Self {
            core: LogQueueCore::new(config, client, execution),
            writes: Arc::new(Mutex::new(QueueState::Dormant)),
            lease: None,
        }
    }
    fn state(&self) -> MutexGuard<'_, QueueState> {
        self.writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn submit(
        &self,
        scope: CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
        outcome: Option<WriteOutcome>,
    ) -> Completion<DeliveryReceipt, WriteError> {
        self.submit_command(
            SubmitCommand::Ready(Box::new(PreparedSpan {
                scope,
                payload,
                parent_info,
            })),
            outcome,
        )
    }

    fn ensure_running(&self, state: &mut MutexGuard<'_, QueueState>) {
        if matches!(**state, QueueState::Dormant) {
            let processor = self.core.clone();
            **state = QueueState::Running(WriteService::new(
                self.core.config.queue_max_size(),
                self.core.execution.clone(),
                move |commands| {
                    let processor = processor.clone();
                    Box::pin(async move { processor.process(commands).await })
                },
            ));
        }
    }

    fn submit_command(
        &self,
        command: SubmitCommand,
        outcome: Option<WriteOutcome>,
    ) -> Completion<DeliveryReceipt, WriteError> {
        let mut state = self.state();
        self.ensure_running(&mut state);
        match &*state {
            QueueState::Running(writes) => match &self.lease {
                Some(lease) => {
                    writes.enqueue_with_lease(command, outcome, Some(lease), |command, error| {
                        self.reject(command, error)
                    })
                }
                None => writes.enqueue_with_rejection(command, outcome, |command, error| {
                    self.reject(command, error)
                }),
            },
            QueueState::Closed => {
                let (sender, mut completion) = Completion::pair();
                completion.record_issuance(Err(crate::EnqueueError::Closed.into()));
                let _ = crate::internal::shared_outcome::WriteResponder::new(sender, outcome)
                    .send(Err(crate::EnqueueError::Closed.into()));
                self.reject(command, crate::EnqueueError::Closed);
                completion
            }
            QueueState::Dormant => unreachable!("worker installed before admission"),
        }
    }
    fn reject(&self, command: SubmitCommand, error: crate::EnqueueError) {
        self.core.dropped_count.fetch_add(1, Ordering::Relaxed);
        if matches!(error, crate::EnqueueError::Full) {
            self.core.log_drop_warning();
        } else {
            warn!(%error, "span submission rejected");
        }
        self.core.dump_dropped_row_if_configured(command);
    }

    pub fn flush(&self) -> Completion<FlushReport, FlushError> {
        match &*self.state() {
            QueueState::Running(writes) => match &self.lease {
                Some(lease) => writes.flush_with_lease(Some(lease)),
                None => writes.flush(),
            },
            state => {
                let (sender, mut completion) = Completion::pair();
                if matches!(state, QueueState::Closed) {
                    completion.record_issuance(Err(crate::EnqueueError::Closed.into()));
                } else {
                    let _ = sender.send(Ok(FlushReport::default()));
                }
                completion
            }
        }
    }
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> Completion<ShutdownReport, ShutdownError> {
        let mut state = self.state();
        if let QueueState::Running(writes) = &*state {
            return writes.shutdown(timeout);
        }
        *state = QueueState::Closed;
        let (sender, completion) = Completion::pair();
        let _ = sender.send(Ok(ShutdownReport::default()));
        completion
    }
    pub fn cancel(&self, error: crate::ExecutionError) {
        let writes = {
            let mut state = self.state();
            if let QueueState::Running(writes) = &*state {
                Some(writes.clone())
            } else {
                *state = QueueState::Closed;
                None
            }
        };
        if let Some(writes) = writes {
            writes.cancel(error);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        match &*self.state() {
            QueueState::Running(writes) => writes.is_empty(),
            _ => true,
        }
    }
    #[cfg(test)]
    pub(crate) fn is_started(&self) -> bool {
        matches!(*self.state(), QueueState::Running(_))
    }
    pub async fn trigger_flush_command(&self) -> Result<()> {
        let token = self.flush();
        if let Err(error) = &*token {
            return Err(error.clone().into());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl DeliveryControl for LogQueue {
    fn flush(&self) -> Completion<FlushReport, FlushError> {
        LogQueue::flush(self)
    }
    async fn trigger_flush(&self) -> Result<()> {
        self.trigger_flush_command().await
    }
}
#[async_trait::async_trait]
impl SpanSubmitter for LogQueue {
    fn operation(&self) -> std::result::Result<SpanOperation, crate::EnqueueError> {
        let lease = {
            let mut state = self.state();
            self.ensure_running(&mut state);
            match &*state {
                QueueState::Running(writes) => writes.lease()?,
                _ => return Err(crate::EnqueueError::Closed),
            }
        };
        let mut scoped = self.clone();
        scoped.lease = Some(lease.clone());
        Ok(SpanOperation::new(Arc::new(scoped), move || lease.revoke()))
    }

    fn submit_deferred(
        &self,
        row_id: String,
        prepare: BoxFuture<'static, Result<PreparedSpan>>,
    ) -> Completion<DeliveryReceipt, WriteError> {
        self.submit_command(SubmitCommand::Deferred { row_id, prepare }, None)
    }

    fn submit(
        &self,
        scope: CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
        outcome: Option<WriteOutcome>,
    ) -> Completion<DeliveryReceipt, WriteError> {
        LogQueue::submit(self, scope, payload, parent_info, outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use url::Url;
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
    fn queue(_server: &MockServer) -> LogQueue {
        LogQueue::new(
            LogQueueConfig::builder().num_retries(0).build(),
            reqwest::Client::new(),
            32,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
        )
    }
    fn scope(server: &MockServer, token: &str) -> CredentialScope {
        CredentialScope::new(
            token.into(),
            "org".into(),
            Some("tenant".into()),
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
    async fn concurrent_first_submissions_share_one_bounded_lazy_worker() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let queue = LogQueue::new(
            LogQueueConfig::builder()
                .queue_max_size(8)
                .num_retries(0)
                .build(),
            reqwest::Client::new(),
            8,
            ExecutionBinding::external(tokio::runtime::Handle::current()),
        );
        assert!(!queue.is_started());
        let tokens = std::thread::scope(|threads| {
            (0..32)
                .map(|number| {
                    let queue = &queue;
                    let server = &server;
                    threads.spawn(move || {
                        queue.submit(
                            scope(server, "token"),
                            payload(&format!("row-{number}"), false),
                            parent(),
                            None,
                        )
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(tokens.iter().filter(|token| token.is_ok()).count(), 8);
        assert!(queue.is_started());
        assert_eq!(queue.flush().await.unwrap().delivered, 8);
        for token in tokens {
            let _ = token.await;
        }
        queue
            .shutdown(std::time::Duration::from_secs(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn late_route_failure_fails_merged_members_and_successful_siblings_then_retires() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(2)
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
        let first = queue.submit(
            scope(&server, "good"),
            payload("merged", false),
            parent(),
            None,
        );
        let update = queue.submit(
            scope(&server, "good"),
            payload("merged", true),
            parent(),
            None,
        );
        let last = queue.submit(
            scope(&server, "bad"),
            payload("other", false),
            parent(),
            None,
        );
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
        let fresh = queue.submit(
            scope(&server, "good"),
            payload("fresh", false),
            parent(),
            None,
        );
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
        let first = queue.submit(
            scope(&server, "token"),
            payload("committed", false),
            parent(),
            None,
        );
        let last = queue.submit(
            scope(&server, "token"),
            payload("late-failure", false),
            parent(),
            None,
        );
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
