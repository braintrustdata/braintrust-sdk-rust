use super::batching::batch_and_serialize_rows;
use super::config::LogQueueConfig;
use super::http::{fetch_version_info, send_batch_with_retry};
use super::merge::merge_row_into;
use super::row_key::RowKey;
use super::worker::{LogCommand, SubmitCommand, WorkerConfig};
use crate::error::{BraintrustError, Result};
use crate::logger::LoginState;
use crate::types::{Logs3Row, ParentSpanInfo, SpanPayload};
use arc_swap::ArcSwap;
use crossbeam::channel::{bounded, Receiver, Sender, TrySendError};
use futures::future::join_all;
use indexmap::IndexMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, Notify, OnceCell};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

/// Lock-free log queue with batching and HTTP dispatch.
///
/// This is the complete log processing system:
/// - Owns the background worker for payload preparation and project registration
/// - Lock-free queue for accumulating rows
/// - Automatic batching and serialization via spawn_blocking
/// - HTTP dispatch with retry logic (concurrent per-chunk batches)
/// - Atomic drain for precise flush boundaries
/// - Matches TypeScript SDK `activeFlush` pattern: only one flush runs at a time
/// - Lazy version fetch for dynamic maxRequestSize and overflow support
#[derive(Clone)]
pub struct LogQueue {
    /// Lock-free queue channel (sender, receiver) pair, atomically swappable
    channel: Arc<ArcSwap<(Sender<Logs3Row>, Receiver<Logs3Row>)>>,
    dropped_count: Arc<AtomicUsize>,
    last_drop_log_time: Arc<AtomicU64>,
    config: LogQueueConfig,
    login_state: LoginState,
    client: reqwest::Client,
    /// Whether a flush is currently in progress (matches TS SDK's activeFlush pattern)
    flush_in_progress: Arc<AtomicBool>,
    /// Notified when a flush completes
    flush_complete: Arc<Notify>,
    /// Lazily cached (max_request_size, can_use_overflow) from GET /version.
    /// Fetched once on first flush and reused thereafter.
    version_info: Arc<OnceCell<(usize, bool)>>,
    /// Channel to the background worker (for submit/flush commands)
    worker_sender: mpsc::Sender<LogCommand>,
    /// Handle to the background worker task
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
        let (row_sender, row_receiver) = bounded(config.queue_max_size());

        // Shared state for the lock-free queue (all Arc-wrapped)
        let channel = Arc::new(ArcSwap::from_pointee((row_sender, row_receiver)));
        let dropped_count = Arc::new(AtomicUsize::new(0));
        let last_drop_log_time = Arc::new(AtomicU64::new(0));
        let flush_in_progress = Arc::new(AtomicBool::new(false));
        let flush_complete = Arc::new(Notify::new());
        let version_info: Arc<OnceCell<(usize, bool)>> = Arc::new(OnceCell::new());

        let (worker_sender, worker_receiver) = mpsc::channel(worker_queue_size.max(32));

        let worker_config = WorkerConfig { app_url };

        // Build a clone of the queue to pass to the worker.
        // The worker uses push() and flush() on this clone to interact with the same
        // underlying lock-free queue. The worker_sender/worker_handle fields in this
        // clone are never used by the worker itself.
        let queue_for_worker = LogQueue {
            channel: channel.clone(),
            dropped_count: dropped_count.clone(),
            last_drop_log_time: last_drop_log_time.clone(),
            config: config.clone(),
            login_state: login_state.clone(),
            client: client.clone(),
            flush_in_progress: flush_in_progress.clone(),
            flush_complete: flush_complete.clone(),
            version_info: version_info.clone(),
            // Dummy values - worker never calls submit() on this clone
            worker_sender: worker_sender.clone(),
            worker_handle: Arc::new(tokio::spawn(async {})),
        };

        let worker_handle = Arc::new(tokio::spawn(super::worker::run_worker(
            login_state.clone(),
            worker_receiver,
            worker_config,
            queue_for_worker,
        )));

        Self {
            channel,
            dropped_count,
            last_drop_log_time,
            config,
            login_state,
            client,
            flush_in_progress,
            flush_complete,
            version_info,
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

    /// Push a row directly to the lock-free queue (used by the worker internally).
    ///
    /// In normal mode (`sync_flush=false`), triggers a background flush.
    /// In sync mode (`sync_flush=true`), just queues - caller must call `flush()`.
    /// Returns immediately in both cases.
    pub(super) fn push(&self, row: Logs3Row) {
        let channel = self.channel.load();
        match channel.0.try_send(row) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                let dropped = self.dropped_count.fetch_add(1, Ordering::Relaxed) + 1;
                self.log_drop_warning(dropped);
                return;
            }
        }

        if !self.config.sync_flush() {
            self.trigger_flush_background();
        }
    }

    /// Trigger a background flush if none is in progress.
    /// Non-blocking - spawns a tokio task if needed.
    /// No-op if no tokio runtime is available (e.g. in sync test contexts).
    fn trigger_flush_background(&self) {
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
            let this = self.clone();
            handle.spawn(async move {
                this.flush_internal().await;
                this.flush_in_progress.store(false, Ordering::SeqCst);
                this.flush_complete.notify_waiters();
            });
        }
    }

    /// Flush all rows in the lock-free queue and wait for completion.
    ///
    /// If a flush is already in progress, waits for it to complete and then
    /// flushes any remaining items (matches TypeScript SDK behavior).
    pub(super) async fn flush(&self) -> std::result::Result<(), anyhow::Error> {
        // Trigger a flush (no-op if one already running)
        self.trigger_flush_background();

        // Wait until queue is empty and no flush is in progress
        loop {
            if !self.flush_in_progress.load(Ordering::SeqCst) {
                if self.is_empty() {
                    return Ok(());
                }
                // Items added since last flush - trigger another round
                self.trigger_flush_background();
            }
            self.flush_complete.notified().await;
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
    async fn flush_internal(&self) {
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
                batch_and_serialize_rows(rows, &config)
            })
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "serialization task failed");
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
        let (new_sender, new_receiver) = bounded(self.config.queue_max_size());
        let old_channel = self.channel.swap(Arc::new((new_sender, new_receiver)));
        let mut items = Vec::new();
        while let Ok(item) = old_channel.1.try_recv() {
            items.push(item);
        }
        items
    }

    /// Get the approximate number of items in the queue.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.channel.load().1.len()
    }

    /// Check if the queue is approximately empty.
    pub fn is_empty(&self) -> bool {
        self.channel.load().1.is_empty()
    }

    /// Get the total number of dropped items.
    #[allow(dead_code)]
    pub fn dropped_count(&self) -> usize {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Log a drop warning with throttling.
    fn log_drop_warning(&self, dropped: usize) {
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
            warn!(
                "Log queue full (size: {}), dropped {} items so far. \
                 Consider increasing BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE.",
                self.config.queue_max_size(),
                dropped
            );
        }
    }

    /// Shutdown: flush all pending data then drop.
    #[allow(dead_code)]
    pub async fn shutdown(&self) {
        let _ = self.flush_all().await;
    }
}

impl Drop for LogQueue {
    fn drop(&mut self) {
        // Only flush on drop if we're logged in (otherwise flush loops indefinitely
        // since flush_internal returns early when not logged in, leaving items in queue)
        if self.is_empty() || !self.login_state.is_logged_in() {
            return;
        }

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(async {
                    // Best-effort flush of the internal queue on drop
                    let _ = self.flush().await;
                });
            });
        } else {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let _ = self.flush().await;
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
        }
    }

    fn make_queue(max_size: usize) -> LogQueue {
        // No login state set - prevents background flushes from making real HTTP calls
        // which would hang tests waiting for connections to non-existent servers.
        let config = LogQueueConfig::builder()
            .queue_max_size(max_size)
            .sync_flush(true) // Disable auto-flush to avoid background HTTP calls
            .build();
        let login_state = LoginState::new();
        let app_url = Url::parse("https://www.example.com").unwrap();
        LogQueue::new(config, login_state, reqwest::Client::new(), app_url, 256)
    }

    #[tokio::test]
    async fn test_push_queues_item() {
        let queue = make_queue(10);
        assert!(queue.is_empty());
        queue.push(make_test_row("1"));
        assert_eq!(queue.len(), 1);
    }

    #[tokio::test]
    async fn test_push_drops_when_full() {
        let queue = make_queue(2);
        queue.push(make_test_row("1"));
        queue.push(make_test_row("2"));
        assert_eq!(queue.len(), 2);

        // This push should be dropped since the queue is full
        queue.push(make_test_row("3"));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped_count(), 1);
    }

    #[tokio::test]
    async fn test_drain_all_returns_all_items() {
        let queue = make_queue(10);
        queue.push(make_test_row("1"));
        queue.push(make_test_row("2"));
        queue.push(make_test_row("3"));

        let drained = queue.drain_all();
        assert_eq!(drained.len(), 3);
        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn test_drain_all_atomic_boundary() {
        // Items pushed after drain starts should go to the new channel, not the drained batch
        let queue = make_queue(10);
        queue.push(make_test_row("1"));
        queue.push(make_test_row("2"));

        let drained = queue.drain_all();
        assert_eq!(drained.len(), 2);

        // Push after drain goes to the new channel
        queue.push(make_test_row("3"));
        assert_eq!(queue.len(), 1);
    }

    #[tokio::test]
    async fn test_push_no_auto_flush_in_sync_mode() {
        // make_queue uses sync_flush=true, so push() should not trigger background flush
        let queue = make_queue(10);

        queue.push(make_test_row("1"));
        // No flush should be in progress - in sync mode user calls flush() explicitly
        assert!(!queue.flush_in_progress.load(Ordering::SeqCst));
        assert_eq!(queue.len(), 1);
    }
}
