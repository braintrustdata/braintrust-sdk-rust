use crate::error::{BraintrustError, Result};
use crate::http::{build_http_client, CaBundleConfig};
use crate::internal::ingest::{EncodedBatch, EncodedRow};
use crate::internal::upload::{Capabilities, UploadCoordinator, UploadPolicy};
use reqwest::{Client, Url};
use serde_json::{Map, Value};
use std::time::Duration;

const DEFAULT_MAX_REQUEST_SIZE: usize = 6 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Logs3UploadResult {
    pub rows_uploaded: usize,
    pub bytes_processed: usize,
    pub requests_sent: usize,
}

#[derive(Debug, Clone)]
pub struct Logs3BatchUploader {
    client: Client,
    api_url: Url,
    api_key: String,
    org_name: Option<String>,
    max_request_size_override: Option<usize>,
    payload_limits: Option<Capabilities>,
}

impl Logs3BatchUploader {
    pub fn new(
        api_url: impl AsRef<str>,
        api_key: impl Into<String>,
        org_name: Option<String>,
    ) -> Result<Self> {
        Self::new_with_ca_bundle_config(
            api_url,
            api_key,
            org_name,
            CaBundleConfig::from_environment(),
        )
    }

    /// Create an uploader with additional PEM certificates to trust.
    ///
    /// This overrides `BRAINTRUST_CUSTOM_CA_BUNDLE` for this uploader.
    pub fn new_with_custom_ca_bundle_pem(
        api_url: impl AsRef<str>,
        api_key: impl Into<String>,
        org_name: Option<String>,
        pem: impl AsRef<[u8]>,
    ) -> Result<Self> {
        Self::new_with_ca_bundle_config(
            api_url,
            api_key,
            org_name,
            Some(CaBundleConfig::from_pem(pem, "custom CA bundle")),
        )
    }

    fn new_with_ca_bundle_config(
        api_url: impl AsRef<str>,
        api_key: impl Into<String>,
        org_name: Option<String>,
        ca_bundle: Option<CaBundleConfig>,
    ) -> Result<Self> {
        let api_url = Url::parse(api_url.as_ref())
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid api_url: {e}")))?;
        let client = build_http_client(Duration::from_secs(30), ca_bundle.as_ref(), None)?;
        Ok(Self {
            client,
            api_url,
            api_key: api_key.into(),
            org_name: org_name.filter(|s| !s.trim().is_empty()),
            max_request_size_override: None,
            payload_limits: None,
        })
    }

    pub fn with_max_request_size_override(mut self, max_bytes: usize) -> Self {
        self.max_request_size_override = Some(max_bytes.max(1));
        self
    }

    pub async fn upload_rows(
        &mut self,
        rows: &[Map<String, Value>],
        batch_max_num_items: usize,
    ) -> Result<Logs3UploadResult> {
        if rows.is_empty() {
            return Ok(Logs3UploadResult::default());
        }

        let coordinator = UploadCoordinator::new(
            self.client.clone(),
            &self.api_url,
            &self.api_key,
            self.org_name.as_deref(),
            UploadPolicy::Standalone,
        )?;
        let limits = match self.payload_limits {
            Some(limits) => limits,
            None => {
                let limits = coordinator
                    .capabilities(DEFAULT_MAX_REQUEST_SIZE, self.max_request_size_override)
                    .await;
                self.payload_limits = Some(limits);
                limits
            }
        };
        let row_payloads = rows
            .iter()
            .map(EncodedRow::from_map)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                BraintrustError::InvalidConfig(format!("failed to serialize logs3 row: {error}"))
            })?;
        let batches = EncodedBatch::partition(
            row_payloads,
            batch_max_num_items.max(1),
            (limits.max_request_size / 2).max(1),
        );

        let mut result = Logs3UploadResult::default();
        for batch in batches {
            let batch_result = coordinator.upload(batch, limits).await?;
            result.bytes_processed += batch_result.bytes_processed;
            result.requests_sent += batch_result.requests_sent;
            result.rows_uploaded += batch_result.rows_uploaded;
        }
        Ok(result)
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

        let result = Logs3BatchUploader::new("https://example.com", "test-key", None);

        match original {
            Some(value) => std::env::set_var(crate::http::CUSTOM_CA_BUNDLE_ENV, value),
            None => std::env::remove_var(crate::http::CUSTOM_CA_BUNDLE_ENV),
        }

        let error = result.expect_err("malformed environment CA bundle should fail");
        assert!(error
            .to_string()
            .contains(crate::http::CUSTOM_CA_BUNDLE_ENV));
    }
}
