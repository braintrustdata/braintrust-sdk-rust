use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{multipart, Client, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::{BraintrustError, Result};
use crate::http::{build_http_client, CaBundleConfig};
use crate::internal::ingest::{EncodedBatch, EncodedRow, UploadPlan, UploadPlanner};
use crate::internal::retry::{jittered_exponential, RetryExecutor};
use crate::types::Logs3OverflowInputRow;

const DEFAULT_MAX_REQUEST_SIZE: usize = 6 * 1024 * 1024;
const LOGS_API_VERSION: u8 = 2;
const LOGS3_OVERFLOW_REFERENCE_TYPE: &str = "logs3_overflow";
const MAX_ATTEMPTS: usize = 5;
const BASE_BACKOFF_MS: u64 = 300;
const MAX_BACKOFF_MS: u64 = 8_000;
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
    payload_limits: Option<PayloadLimits>,
}

#[derive(Debug, Clone)]
struct PayloadLimits {
    max_request_size: usize,
    can_use_overflow: bool,
}

#[derive(Debug, Deserialize)]
struct VersionInfo {
    #[serde(default)]
    logs3_payload_max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum OverflowMethod {
    Put,
    Post,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Logs3OverflowUpload {
    method: OverflowMethod,
    signed_url: String,
    headers: Option<HashMap<String, String>>,
    fields: Option<HashMap<String, String>>,
    key: String,
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

        let limits = self.get_payload_limits().await;
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
            let batch_result = self.submit_row_batch(batch, &limits).await?;
            result.bytes_processed += batch_result.bytes_processed;
            result.requests_sent += batch_result.requests_sent;
            result.rows_uploaded += batch_result.rows_uploaded;
        }
        Ok(result)
    }

    async fn get_payload_limits(&mut self) -> PayloadLimits {
        if let Some(existing) = &self.payload_limits {
            return existing.clone();
        }
        let fetched = self.fetch_payload_limits().await;
        self.payload_limits = Some(fetched.clone());
        fetched
    }

    async fn fetch_payload_limits(&self) -> PayloadLimits {
        let mut server_limit: Option<usize> = None;
        if let Ok(url) = self.api_url.join("version") {
            let mut request = self.client.get(url).bearer_auth(&self.api_key);
            if let Some(org_name) = &self.org_name {
                request = request.header("x-bt-org-name", org_name);
            }
            if let Ok(response) = request.send().await {
                if response.status().is_success() {
                    if let Ok(version) = response.json::<VersionInfo>().await {
                        server_limit = version.logs3_payload_max_bytes.filter(|v| *v > 0);
                    }
                }
            }
        }

        let can_use_overflow = server_limit.is_some();
        let mut max_request_size = DEFAULT_MAX_REQUEST_SIZE;
        if let Some(override_bytes) = self.max_request_size_override {
            max_request_size = if let Some(server_limit) = server_limit {
                override_bytes.min(server_limit)
            } else {
                override_bytes
            };
        } else if let Some(server_limit) = server_limit {
            max_request_size = server_limit;
        }
        PayloadLimits {
            max_request_size: max_request_size.max(1),
            can_use_overflow,
        }
    }

    async fn submit_row_batch(
        &self,
        batch: EncodedBatch,
        limits: &PayloadLimits,
    ) -> Result<Logs3UploadResult> {
        let planner = UploadPlanner {
            max_request_bytes: limits.max_request_size,
            overflow: limits.can_use_overflow,
        };
        let mut pending = vec![planner.plan(batch)?];
        let mut result = Logs3UploadResult::default();
        while let Some(plan) = pending.pop() {
            match plan {
                UploadPlan::Split(left, right) => {
                    pending.push(planner.plan(right)?);
                    pending.push(planner.plan(left)?);
                }
                UploadPlan::Overflow(batch) => {
                    let uploaded = self.submit_overflow(&batch).await?;
                    result.rows_uploaded += uploaded.rows_uploaded;
                    result.bytes_processed += uploaded.bytes_processed;
                    result.requests_sent += uploaded.requests_sent;
                }
                UploadPlan::Direct(batch) => {
                    let (posted, attempts) =
                        run_with_backoff(|| self.post_logs3_raw(&batch.data)).await;
                    result.requests_sent += attempts;
                    match posted {
                        Ok(()) => {
                            result.rows_uploaded += batch.len();
                            result.bytes_processed += batch.data.len();
                        }
                        Err(BraintrustError::Api { status: 413, .. }) => {
                            pending.push(planner.rejected(batch)?)
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        Ok(result)
    }

    async fn submit_overflow(&self, batch: &EncodedBatch) -> Result<Logs3UploadResult> {
        let mut requests_sent = 0;
        let (upload_result, attempts) = run_with_backoff(|| {
            self.request_overflow_upload(&batch.overflow_rows, batch.data.len())
        })
        .await;
        requests_sent += attempts;
        let upload = upload_result?;
        let (upload_result, attempts) =
            run_with_backoff(|| self.upload_overflow_payload(&upload, &batch.data)).await;
        requests_sent += attempts;
        upload_result?;
        let reference = serde_json::to_vec(&json!({
            "rows": {"type": LOGS3_OVERFLOW_REFERENCE_TYPE, "key": upload.key},
            "api_version": LOGS_API_VERSION,
        }))
        .map_err(|error| BraintrustError::InvalidConfig(error.to_string()))?;
        let (posted, attempts) = run_with_backoff(|| self.post_logs3_raw(&reference)).await;
        requests_sent += attempts;
        posted?;
        Ok(Logs3UploadResult {
            rows_uploaded: batch.len(),
            bytes_processed: batch.data.len(),
            requests_sent,
        })
    }

    async fn request_overflow_upload(
        &self,
        rows: &[Logs3OverflowInputRow],
        payload_bytes: usize,
    ) -> Result<Logs3OverflowUpload> {
        let url = self
            .api_url
            .join("logs3/overflow")
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid overflow URL: {e}")))?;

        let request_body = json!({
            "content_type": "application/json",
            "size_bytes": payload_bytes,
            "rows": rows,
        });
        let mut request = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&request_body);
        if let Some(org_name) = &self.org_name {
            request = request.header("x-bt-org-name", org_name);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(BraintrustError::Api { status, message });
        }
        response
            .json::<Logs3OverflowUpload>()
            .await
            .map_err(|e| BraintrustError::Api {
                status: 200,
                message: format!("failed to parse logs3 overflow response: {e}"),
            })
    }

    async fn upload_overflow_payload(
        &self,
        upload: &Logs3OverflowUpload,
        payload: &[u8],
    ) -> Result<()> {
        let signed_url = Url::parse(&upload.signed_url).map_err(|e| {
            BraintrustError::InvalidConfig(format!("invalid logs3 overflow signed URL: {e}"))
        })?;
        match upload.method {
            OverflowMethod::Post => {
                let fields = upload.fields.clone().ok_or_else(|| {
                    BraintrustError::InvalidConfig(
                        "logs3 overflow POST upload missing form fields".to_string(),
                    )
                })?;
                let content_type = fields
                    .get("Content-Type")
                    .cloned()
                    .unwrap_or_else(|| "application/json".to_string());
                let mut form = multipart::Form::new();
                for (key, value) in &fields {
                    form = form.text(key.clone(), value.clone());
                }
                let file_part = multipart::Part::bytes(payload.to_vec())
                    .mime_str(&content_type)
                    .map_err(|e| {
                        BraintrustError::InvalidConfig(format!(
                            "invalid overflow content-type '{content_type}': {e}"
                        ))
                    })?;
                form = form.part("file", file_part);

                let mut request = self.client.post(signed_url).multipart(form);
                let mut headers = header_map_from_pairs(upload.headers.as_ref())?;
                headers.remove("content-type");
                if !headers.is_empty() {
                    request = request.headers(headers);
                }
                let response = request.send().await?;
                if !response.status().is_success() {
                    let status = response.status().as_u16();
                    let message = response.text().await.unwrap_or_default();
                    return Err(BraintrustError::Api { status, message });
                }
            }
            OverflowMethod::Put => {
                let mut headers = header_map_from_pairs(upload.headers.as_ref())?;
                if upload.signed_url.contains("blob.core.windows.net")
                    && !headers.contains_key("x-ms-blob-type")
                {
                    headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
                }

                let mut request = self.client.put(signed_url).body(payload.to_vec());
                if !headers.is_empty() {
                    request = request.headers(headers);
                }
                let response = request.send().await?;
                if !response.status().is_success() {
                    let status = response.status().as_u16();
                    let message = response.text().await.unwrap_or_default();
                    return Err(BraintrustError::Api { status, message });
                }
            }
        }
        Ok(())
    }

    async fn post_logs3_raw(&self, payload: &[u8]) -> Result<()> {
        let url = self
            .api_url
            .join("logs3")
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid logs3 URL: {e}")))?;
        let mut request = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .body(payload.to_vec());
        if let Some(org_name) = &self.org_name {
            request = request.header("x-bt-org-name", org_name);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(BraintrustError::Api { status, message });
        }
        Ok(())
    }
}

fn header_map_from_pairs(headers: Option<&HashMap<String, String>>) -> Result<HeaderMap> {
    let mut out = HeaderMap::new();
    if let Some(headers) = headers {
        for (key, value) in headers {
            let name = HeaderName::from_bytes(key.as_bytes()).map_err(|e| {
                BraintrustError::InvalidConfig(format!("invalid HTTP header name '{key}': {e}"))
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|e| {
                BraintrustError::InvalidConfig(format!(
                    "invalid HTTP header value for '{key}': {e}"
                ))
            })?;
            out.insert(name, header_value);
        }
    }
    Ok(out)
}

fn should_retry(err: &BraintrustError) -> bool {
    match err {
        BraintrustError::Http(http_err) => http_err.is_timeout() || http_err.is_connect(),
        BraintrustError::Network(_)
        | BraintrustError::ChannelClosed
        | BraintrustError::Background(_) => true,
        BraintrustError::Api { status, .. } => *status == 429 || (*status >= 500 && *status <= 599),
        BraintrustError::InvalidConfig(_) | BraintrustError::StreamAggregation(_) => false,
    }
}

async fn run_with_backoff<T, F, Fut>(mut operation: F) -> (Result<T>, usize)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let outcome = RetryExecutor::new(
        MAX_ATTEMPTS - 1,
        jittered_exponential(
            Duration::from_millis(BASE_BACKOFF_MS),
            Duration::from_millis(MAX_BACKOFF_MS),
            0.2,
        ),
    )
    .run(|_| operation(), should_retry, |_, _, _| {})
    .await;
    (outcome.result, outcome.attempts)
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
