//! Reliable ingestion of caller-owned batches, separate from resource logging.
//!
//! Use this adapter for bulk imports whose batch membership belongs to the
//! application. It shares SDK encoding, splitting, retry and overflow handling.
//! `api::Client::logs3()` remains the one-attempt endpoint surface.
//!
//! ```no_run
//! use braintrust_sdk_rust::{api::Client, ingest::BatchUploader};
//! use serde_json::{Map, Value};
//! use std::time::Duration;
//! # async fn example() -> anyhow::Result<()> {
//! let uploader = BatchUploader::new(Client::builder().api_key("your-api-key").build()?)?;
//! let row = Map::from_iter([
//!     ("id".into(), Value::String("row-id".into())),
//!     ("dataset_id".into(), Value::String("dataset-id".into())),
//!     ("input".into(), Value::String("hello".into())),
//! ]);
//! let delivery = uploader.upload_rows(vec![row], 100);
//! assert!(delivery.is_ok()); // admission has already happened
//! let result = delivery.await?;
//! assert_eq!(result.rows_uploaded(), 1);
//! uploader.shutdown(Duration::from_secs(10)).await?;
//! # Ok(())
//! # }
//! ```
use crate::internal::execution::ExecutionBinding;
use crate::internal::shared_outcome::{SharedOutcome, SharedResponder};
use crate::internal::{
    ingest::{EncodedBatch, EncodedRow},
    upload::{Capabilities, UploadCoordinator, UploadPolicy},
};
use crate::{api, BraintrustError, Completion, ShutdownError};
use serde_json::{Map, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct UploadResult {
    pub(crate) rows_uploaded: usize,
    pub(crate) bytes_processed: usize,
    pub(crate) requests_sent: usize,
}

type ShutdownOutcome = SharedOutcome<(), ShutdownError>;

/// Concrete bulk-ingestion client. Clones share a runtime, admission capacity,
/// immutable credentials, capability discovery and shutdown.
#[derive(Clone)]
pub struct BatchUploader {
    api: api::Client,
    limits: Arc<Mutex<Option<Capabilities>>>,
    execution: ExecutionBinding,
    shutdown: Arc<std::sync::Mutex<Option<ShutdownOutcome>>>,
}
impl BatchUploader {
    /// Use an explicitly configured generated client on an SDK-owned runtime.
    pub fn new(api: api::Client) -> Result<Self, BraintrustError> {
        Self::with_runtime(api, None)
    }
    /// Select the same explicit-or-owned runtime policy as the resource client.
    pub fn with_runtime(
        api: api::Client,
        runtime: Option<tokio::runtime::Handle>,
    ) -> Result<Self, BraintrustError> {
        Ok(Self {
            api,
            execution: ExecutionBinding::new(runtime)?,
            limits: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(std::sync::Mutex::new(None)),
        })
    }
    /// Submit this owned batch immediately. Await for full delivery; dropping
    /// the observer leaves accepted work running. A terminal error belongs only
    /// to this operation and does not prevent the next batch from proceeding.
    pub fn upload_rows(
        &self,
        rows: Vec<Map<String, Value>>,
        batch_max_num_items: usize,
    ) -> Completion<UploadResult, BraintrustError> {
        let cache = self.limits.clone();
        let api = self.api.clone();
        self.execution.response(async move {
            if rows.is_empty() {
                return Ok(UploadResult::default());
            }
            let coordinator = UploadCoordinator::new(api, UploadPolicy::Standalone);
            let limits = {
                let mut cached = cache.lock().await;
                match *cached {
                    Some(limits) => limits,
                    None => {
                        let limits = coordinator.capabilities(6 * 1024 * 1024, None).await;
                        *cached = Some(limits);
                        limits
                    }
                }
            };
            let row_payloads = rows
                .iter()
                .map(EncodedRow::from_map)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| {
                    BraintrustError::InvalidConfig(format!(
                        "failed to serialize logs3 row: {error}"
                    ))
                })?;
            let batches = EncodedBatch::partition(
                row_payloads,
                batch_max_num_items.max(1),
                (limits.max_request_size / 2).max(1),
            );

            let mut result = UploadResult::default();
            for batch in batches {
                let batch_result = coordinator.upload(batch, limits).await?;
                result.bytes_processed += batch_result.bytes_processed;
                result.requests_sent += batch_result.requests_sent;
                result.rows_uploaded += batch_result.rows_uploaded;
            }
            Ok(result)
        })
    }
    /// Close admission and drain accepted batch operations. Individual upload
    /// failures stay on their tokens; this reports execution drain/timeout.
    pub fn shutdown(&self, timeout: Duration) -> Completion<(), ShutdownError> {
        let mut state = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(outcome) = &*state {
            return outcome.observe();
        }
        let outcome = SharedOutcome::default();
        let (sender, completion) = Completion::pair();
        let responder = SharedResponder::new(sender, Some(outcome.clone()));
        *state = Some(outcome);
        self.execution.close_responses();
        let execution = self.execution.clone();
        let deadline = tokio::time::Instant::now().checked_add(timeout);
        self.execution.spawn(async move {
            let work = execution.responses_drained();
            let result = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, work).await,
                None => Ok(work.await),
            };
            let result = match result {
                Ok(result) => result.map_err(ShutdownError::Execution),
                Err(_) => {
                    execution.cancel_responses(crate::ExecutionError::TimedOut);
                    Err(ShutdownError::Execution(crate::ExecutionError::TimedOut))
                }
            };
            let _ = responder.send(result);
        });
        completion
    }
}

impl UploadResult {
    pub fn rows_uploaded(&self) -> usize {
        self.rows_uploaded
    }
    pub fn bytes_processed(&self) -> usize {
        self.bytes_processed
    }
    pub fn requests_sent(&self) -> usize {
        self.requests_sent
    }
}
