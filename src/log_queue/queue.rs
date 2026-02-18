use super::batching::batch_and_serialize_rows;
use super::config::LogQueueConfig;
use super::http::{fetch_version_info, save_payload_debug, send_batch_with_retry};
use super::merge::merge_row_into;
use super::row_key::RowKey;
use super::worker::{LogCommand, SubmitCommand, WorkerConfig};
use crate::error::{BraintrustError, Result};
use crate::logger::LoginState;
use crate::types::{Logs3Row, ParentSpanInfo, SpanPayload};
use arc_swap::ArcSwap;
use crossbeam::channel::{bounded, unbounded, Receiver, Sender, TrySendError};
use futures::future::join_all;
use indexmap::IndexMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, Notify, OnceCell};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

/// Create a row channel (bounded or unbounded) based on the queue config.
fn make_row_channel(config: &LogQueueConfig) -> (Sender<Logs3Row>, Receiver<Logs3Row>) {
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
    channel: ArcSwap<(Sender<Logs3Row>, Receiver<Logs3Row>)>,
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    pub(super) config: LogQueueConfig,
    pub(super) login_state: LoginState,
    client: reqwest::Client,
    /// Whether a flush is currently in progress (matches TS SDK's activeFlush pattern).
    flush_in_progress: AtomicBool,
    /// Notified when a flush completes.
    flush_complete: Notify,
    /// Lazily cached (max_request_size, can_use_overflow) from GET /version.
    /// Fetched once on first flush and reused thereafter.
    version_info: OnceCell<(usize, bool)>,
}

impl LogQueueCore {
    fn new(config: LogQueueConfig, login_state: LoginState, client: reqwest::Client) -> Arc<Self> {
        let (row_sender, row_receiver) = make_row_channel(&config);
        Arc::new(Self {
            channel: ArcSwap::from_pointee((row_sender, row_receiver)),
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            config,
            login_state,
            client,
            flush_in_progress: AtomicBool::new(false),
            flush_complete: Notify::new(),
            version_info: OnceCell::new(),
        })
    }

    /// Push a row directly to the lock-free queue.
    ///
    /// Called by the worker after building the row. In normal mode (`sync_flush=false`),
    /// triggers a background flush. In sync mode, caller must call `flush()` explicitly.
    pub(super) fn push(self: &Arc<Self>, row: Logs3Row) {
        let channel = self.channel.load();
        match channel.0.try_send(row) {
            Ok(()) => {}
            Err(TrySendError::Full(row)) => {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
                self.log_drop_warning();
                self.dump_dropped_row_if_configured(row);
                return;
            }
            Err(TrySendError::Disconnected(_)) => {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
                self.log_drop_warning();
                return;
            }
        }

        if !self.config.sync_flush() {
            self.trigger_flush_background();
        }
    }

    /// Trigger a background flush if none is in progress.
    /// Non-blocking — spawns a tokio task if needed.
    /// No-op if no tokio runtime is available (e.g. in sync test contexts).
    fn trigger_flush_background(self: &Arc<Self>) {
        if self
            .flush_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                // No runtime available; reset flag and return
                self.flush_in_progress.store(false, Ordering::SeqCst);
                return;
            };
            let core = self.clone();
            handle.spawn(async move {
                core.flush_internal().await;
                core.flush_in_progress.store(false, Ordering::SeqCst);
                core.flush_complete.notify_waiters();
            });
        }
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
        // Trigger a flush (no-op if one already running)
        self.trigger_flush_background();

        // Wait until the queue is empty and no flush is in progress.
        // Pre-register the notification before checking state to avoid a race where
        // flush_complete fires between the flag check and notified().await.
        loop {
            let notified = self.flush_complete.notified();
            tokio::pin!(notified);
            // Enable before state check so we won't miss a notify that fires first.
            notified.as_mut().enable();

            if !self.flush_in_progress.load(Ordering::SeqCst) {
                if self.is_empty() {
                    return Ok(());
                }
                // Items added since last flush — trigger another round.
                self.trigger_flush_background();
            }
            notified.await;
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

    /// Internal flush implementation: drain → chunk → merge → batch → send.
    ///
    /// Rows are drained atomically, then processed in sequential chunks of
    /// `min(batch_max_items, flush_chunk_size)` (matching TypeScript SDK).
    /// Within each chunk, batches are sent concurrently.
    async fn flush_internal(self: &Arc<Self>) {
        let rows = self.drain_all();
        if rows.is_empty() {
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

    /// Drain all rows from the queue with precise flush boundaries.
    ///
    /// Atomically swaps in a new channel using lock-free CAS, then drains the old channel.
    fn drain_all(&self) -> Vec<Logs3Row> {
        let (new_sender, new_receiver) = make_row_channel(&self.config);
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

    /// If debug directories are configured, serialize the dropped row and write it to disk.
    /// Spawns a background task — best-effort, non-blocking.
    fn dump_dropped_row_if_configured(&self, row: Logs3Row) {
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
            let payload = match serde_json::to_vec(&serde_json::json!({
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
                if let Err(e) = save_payload_debug(&payload, Some(dir), "dropped") {
                    warn!(error = %e, "failed to write dropped row to debug dir");
                }
            }
        });
    }
}

/// Lock-free log queue with batching and HTTP dispatch.
///
/// This is the complete log processing system:
/// - Owns the background worker for payload preparation and project registration
/// - Lock-free queue for accumulating rows (bounded or unbounded)
/// - Automatic batching and serialization via spawn_blocking
/// - HTTP dispatch with retry logic (concurrent per-chunk batches)
/// - Atomic drain for precise flush boundaries
/// - Matches TypeScript SDK `activeFlush` pattern: only one flush runs at a time
/// - Lazy version fetch for dynamic maxRequestSize and overflow support
#[derive(Clone)]
pub struct LogQueue {
    /// All shared state, also held by the background worker via Arc.
    /// The worker never calls submit() on this — it only uses push() and flush()
    /// directly on the core.
    core: Arc<LogQueueCore>,
    /// Channel to the background worker (for submit/flush commands).
    worker_sender: mpsc::Sender<LogCommand>,
    /// Handle to the background worker task.
    #[allow(dead_code)]
    worker_handle: Arc<JoinHandle<()>>,
}

impl LogQueue {
    /// Create a new log queue that spawns and owns the background worker.
    ///
    /// The worker handles payload preparation, project registration, and dispatches
    /// rows into the internal lock-free queue for batching and HTTP dispatch.
    pub fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: reqwest::Client,
        app_url: Url,
        worker_queue_size: usize,
    ) -> Self {
        let core = LogQueueCore::new(config, login_state.clone(), client.clone());

        let (worker_sender, worker_receiver) = mpsc::channel(worker_queue_size.max(32));

        let worker_config = WorkerConfig {
            app_url,
            client: client.clone(),
        };

        let worker_handle = Arc::new(tokio::spawn(super::worker::run_worker(
            worker_receiver,
            worker_config,
            core.clone(),
        )));

        Self {
            core,
            worker_sender,
            worker_handle,
        }
    }

    /// Submit a span payload for processing by the background worker.
    ///
    /// The worker handles project registration and row preparation,
    /// then pushes the row into the lock-free queue for batching and HTTP dispatch.
    pub async fn submit(
        &self,
        token: impl Into<String>,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        let cmd = LogCommand::Submit(Box::new(SubmitCommand {
            token: token.into(),
            payload,
            parent_info,
        }));
        self.worker_sender
            .send(cmd)
            .await
            .map_err(|_| BraintrustError::ChannelClosed)
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

    fn make_test_row(id: &str) -> Logs3Row {
        Logs3Row {
            id: id.to_string(),
            span_id: id.to_string(),
            is_merge: None,
            root_span_id: "root".to_string(),
            span_parents: None,
            destination: crate::types::LogDestination::experiment("test"),
            org_id: "org".to_string(),
            org_name: Some("test-org".to_string()),
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
            created: chrono::Utc::now(),
            xact_id: None,
            object_delete: None,
            audit_source: None,
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
        let app_url = Url::parse("https://www.example.com").unwrap();
        LogQueue::new(config, login_state, reqwest::Client::new(), app_url, 256)
    }

    #[tokio::test]
    async fn test_push_queues_item() {
        let queue = make_queue(10);
        assert!(queue.core.is_empty());
        queue.core.push(make_test_row("1"));
        assert_eq!(queue.core.len(), 1);
    }

    #[tokio::test]
    async fn test_push_drops_when_full() {
        let queue = make_queue(2);
        queue.core.push(make_test_row("1"));
        queue.core.push(make_test_row("2"));
        assert_eq!(queue.core.len(), 2);

        // This push should be dropped since the queue is full
        queue.core.push(make_test_row("3"));
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
        let app_url = Url::parse("https://www.example.com").unwrap();
        let queue = LogQueue::new(config, login_state, reqwest::Client::new(), app_url, 256);

        queue.core.push(make_test_row("1")); // fills queue

        // First drop: warning fires (last_drop_log_time=0 is always past throttle),
        // resets counter to 0 and records timestamp.
        queue.core.push(make_test_row("2")); // dropped, count reset to 0 after warning
        assert_eq!(queue.core.dropped_count(), 0);

        // Subsequent drops within the same throttle window accumulate without reset.
        queue.core.push(make_test_row("3")); // dropped, counter now 1
        queue.core.push(make_test_row("4")); // dropped, counter now 2
        assert_eq!(queue.core.dropped_count(), 2);
    }

    #[tokio::test]
    async fn test_unbounded_queue_never_drops() {
        let config = LogQueueConfig::builder()
            .enforce_queue_size_limit(false)
            .sync_flush(true)
            .build();
        let login_state = LoginState::new();
        let app_url = Url::parse("https://www.example.com").unwrap();
        let queue = LogQueue::new(config, login_state, reqwest::Client::new(), app_url, 256);

        // Push many more items than the default queue_max_size
        for i in 0..20_000 {
            queue.core.push(make_test_row(&i.to_string()));
        }
        assert_eq!(queue.core.len(), 20_000);
        assert_eq!(queue.core.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_drain_all_returns_all_items() {
        let queue = make_queue(10);
        queue.core.push(make_test_row("1"));
        queue.core.push(make_test_row("2"));
        queue.core.push(make_test_row("3"));

        let drained = queue.core.drain_all();
        assert_eq!(drained.len(), 3);
        assert!(queue.core.is_empty());
    }

    #[tokio::test]
    async fn test_drain_all_atomic_boundary() {
        // Items pushed after drain starts should go to the new channel, not the drained batch
        let queue = make_queue(10);
        queue.core.push(make_test_row("1"));
        queue.core.push(make_test_row("2"));

        let drained = queue.core.drain_all();
        assert_eq!(drained.len(), 2);

        // Push after drain goes to the new channel
        queue.core.push(make_test_row("3"));
        assert_eq!(queue.core.len(), 1);
    }

    #[tokio::test]
    async fn test_push_no_auto_flush_in_sync_mode() {
        // make_queue uses sync_flush=true, so push() should not trigger background flush
        let queue = make_queue(10);

        queue.core.push(make_test_row("1"));
        // No flush should be in progress - in sync mode user calls flush() explicitly
        assert!(!queue.core.flush_in_progress.load(Ordering::SeqCst));
        assert_eq!(queue.core.len(), 1);
    }
}
