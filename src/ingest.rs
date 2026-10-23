//! Reliable ingestion of caller-owned batches, separate from resource logging.
//!
//! Use this adapter for bulk imports whose batch membership belongs to the
//! application. It shares SDK encoding, splitting, retry and overflow handling.
//! `api::Client::logs3()` remains the one-attempt endpoint surface.
//!
//! ```no_run
//! use braintrust_sdk_rust::{api::Config, ingest::BatchUploader};
//! use serde_json::{Map, Value};
//! use std::time::Duration;
//! # async fn example() -> anyhow::Result<()> {
//! let uploader = BatchUploader::new(Config::new("your-api-key"))?;
//! let row = Map::from_iter([
//!     ("id".into(), Value::String("row-id".into())),
//!     ("dataset_id".into(), Value::String("dataset-id".into())),
//!     ("input".into(), Value::String("hello".into())),
//! ]);
//! let delivery = uploader.upload_rows(vec![row], 100);
//! assert!(delivery.is_ok()); // admission has already happened
//! let result = delivery.await?;
//! assert_eq!(result.rows_uploaded, 1);
//! uploader.shutdown(Duration::from_secs(10)).await?;
//! # Ok(())
//! # }
//! ```
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
    pub rows_uploaded: usize,
    pub bytes_processed: usize,
    pub requests_sent: usize,
}

/// Concrete bulk-ingestion client. Clones share a runtime, admission capacity,
/// immutable credentials, capability discovery and shutdown.
#[derive(Clone)]
pub struct BatchUploader {
    api: api::Client,
    limits: Arc<Mutex<Option<Capabilities>>>,
}
impl BatchUploader {
    /// Validate explicit configuration without login; runtime selection matches
    /// `api::Client`: None owns a runtime, Some uses the caller's handle.
    pub fn new(config: api::Config) -> Result<Self, api::Error> {
        Ok(Self {
            api: api::Client::new(config)?,
            limits: Arc::new(Mutex::new(None)),
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
        self.api.submit_operation(move |transport| async move {
            if rows.is_empty() {
                return Ok(UploadResult::default());
            }
            let coordinator = UploadCoordinator::for_transport(transport, UploadPolicy::Standalone);
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
        self.api.shutdown(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn uploader_rejects_malformed_environment_ca_bundle() {
        let _lock = crate::http::tests::ENV_LOCK.lock().await;
        let original = std::env::var_os(crate::http::CUSTOM_CA_BUNDLE_ENV);
        std::env::set_var(
            crate::http::CUSTOM_CA_BUNDLE_ENV,
            "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
        );
        let result = BatchUploader::new(api::Config::new("test-key"));
        match original {
            Some(value) => std::env::set_var(crate::http::CUSTOM_CA_BUNDLE_ENV, value),
            None => std::env::remove_var(crate::http::CUSTOM_CA_BUNDLE_ENV),
        }
        assert!(result
            .err()
            .expect("malformed CA bundle should fail")
            .to_string()
            .contains(crate::http::CUSTOM_CA_BUNDLE_ENV));
    }
}
