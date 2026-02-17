use std::time::Duration;

use backoff::backoff::Backoff;
use backoff::ExponentialBackoffBuilder;
use reqwest::{Client, Url};
use serde_json::{json, Map, Value};
use tokio::time::sleep;

use super::batching::batch_items;
use super::http::{request_overflow_upload, upload_overflow_payload};
use crate::error::{BraintrustError, Result};
use crate::types::{Logs3OverflowInputRow, Logs3OverflowInputRowMeta, OBJECT_ID_KEYS};

const LOGS_API_VERSION: u8 = 2;
const LOGS3_OVERFLOW_REFERENCE_TYPE: &str = "logs3_overflow";
const MAX_ATTEMPTS: usize = 5;
const BASE_BACKOFF_MS: u64 = 300;
const MAX_BACKOFF_MS: u64 = 8_000;
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
    payload_limits: Option<PayloadLimits>,
}

#[derive(Debug, Clone)]
struct PayloadLimits {
    max_request_size: usize,
    can_use_overflow: bool,
}

#[derive(Debug, Clone)]
struct RowPayload {
    row_json: String,
    row_bytes: usize,
    overflow_meta: Logs3OverflowInputRow,
}

impl Logs3BatchUploader {
    pub fn new(
        api_url: impl AsRef<str>,
        api_key: impl Into<String>,
        org_name: Option<String>,
    ) -> Result<Self> {
        let api_url = Url::parse(api_url.as_ref())
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid api_url: {e}")))?;
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid HTTP client: {e}")))?;
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
        let mut row_payloads = Vec::with_capacity(rows.len());
        for row in rows {
            row_payloads.push(RowPayload::from_row(row)?);
        }

        let batch_limit_rows = batch_max_num_items.max(1);
        let batch_limit_bytes = (limits.max_request_size / 2).max(1);
        let batches = batch_items(row_payloads, batch_limit_rows, batch_limit_bytes, |item| {
            item.row_bytes
        });

        let mut result = Logs3UploadResult::default();
        for batch in batches {
            let batch_result = self.submit_row_batch(&batch, &limits).await?;
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
        let (server_max, can_use_overflow) = super::http::fetch_version_info(
            &self.client,
            &self.api_url,
            &self.api_key,
            self.org_name.as_deref(),
        )
        .await;

        // fetch_version_info returns can_use_overflow=true only if the server
        // responded with a positive logs3_payload_max_bytes
        let server_limit = if can_use_overflow {
            Some(server_max)
        } else {
            None
        };

        let mut max_request_size = DEFAULT_MAX_REQUEST_SIZE;
        if let Some(override_bytes) = self.max_request_size_override {
            max_request_size = if let Some(server_limit) = server_limit {
                override_bytes.min(server_limit)
            } else {
                override_bytes
            };
        } else if let Some(limit) = server_limit {
            max_request_size = limit;
        }
        PayloadLimits {
            max_request_size: max_request_size.max(1),
            can_use_overflow,
        }
    }

    async fn submit_row_batch(
        &self,
        items: &[RowPayload],
        limits: &PayloadLimits,
    ) -> Result<Logs3UploadResult> {
        if items.is_empty() {
            return Ok(Logs3UploadResult::default());
        }
        let mut pending: Vec<Vec<RowPayload>> = vec![items.to_vec()];
        let mut result = Logs3UploadResult::default();

        while let Some(chunk) = pending.pop() {
            if chunk.is_empty() {
                continue;
            }
            let payload = construct_logs3_payload(&chunk);
            let payload_bytes = payload.len();

            if limits.can_use_overflow && payload_bytes > limits.max_request_size {
                let overflow_result = self.submit_overflow(&chunk, payload, payload_bytes).await?;
                result.rows_uploaded += overflow_result.rows_uploaded;
                result.bytes_processed += overflow_result.bytes_processed;
                result.requests_sent += overflow_result.requests_sent;
                continue;
            }

            let (post_result, post_attempts) =
                run_with_backoff(|| self.post_logs3_raw(&payload)).await;
            result.requests_sent += post_attempts;
            match post_result {
                Ok(()) => {
                    result.rows_uploaded += chunk.len();
                    result.bytes_processed += payload_bytes;
                }
                Err(BraintrustError::Api { status, .. }) if status == 413 => {
                    if limits.can_use_overflow {
                        let overflow_result = self
                            .submit_overflow(&chunk, payload.clone(), payload_bytes)
                            .await?;
                        result.rows_uploaded += overflow_result.rows_uploaded;
                        result.bytes_processed += overflow_result.bytes_processed;
                        result.requests_sent += overflow_result.requests_sent;
                    } else if chunk.len() > 1 {
                        let mid = chunk.len() / 2;
                        pending.push(chunk[mid..].to_vec());
                        pending.push(chunk[..mid].to_vec());
                    } else {
                        return Err(BraintrustError::Api {
                            status,
                            message:
                                "single row exceeds server payload limit and overflow is unavailable"
                                    .to_string(),
                        });
                    }
                }
                Err(err) => return Err(err),
            }
        }

        Ok(result)
    }

    async fn submit_overflow(
        &self,
        items: &[RowPayload],
        payload: String,
        payload_bytes: usize,
    ) -> Result<Logs3UploadResult> {
        let overflow_rows: Vec<Logs3OverflowInputRow> = items
            .iter()
            .map(|item| item.overflow_meta.clone())
            .collect();
        let mut requests_sent = 0usize;

        let payload_bytes_vec = payload.as_bytes().to_vec();

        let upload = self
            .request_overflow_with_retry(&overflow_rows, &payload_bytes_vec, &mut requests_sent)
            .await?;

        self.upload_payload_with_retry(&upload, &payload_bytes_vec, &mut requests_sent)
            .await?;

        let overflow_reference = json!({
            "rows": {
                "type": LOGS3_OVERFLOW_REFERENCE_TYPE,
                "key": upload.key,
            },
            "api_version": LOGS_API_VERSION,
        })
        .to_string();
        let (post_result, post_attempts) =
            run_with_backoff(|| self.post_logs3_raw(&overflow_reference)).await;
        requests_sent += post_attempts;
        post_result?;

        Ok(Logs3UploadResult {
            rows_uploaded: items.len(),
            bytes_processed: payload_bytes,
            requests_sent,
        })
    }

    async fn request_overflow_with_retry(
        &self,
        overflow_rows: &[Logs3OverflowInputRow],
        payload_bytes: &[u8],
        requests_sent: &mut usize,
    ) -> Result<crate::types::Logs3OverflowUpload> {
        let mut attempts = 0usize;
        let mut backoff = new_backoff();
        loop {
            attempts += 1;
            let result = request_overflow_upload(
                &self.client,
                &self.api_url,
                &self.api_key,
                payload_bytes,
                overflow_rows,
            )
            .await;
            match result {
                Ok(upload) => {
                    *requests_sent += attempts;
                    return Ok(upload);
                }
                Err(e) => {
                    let bt_err = BraintrustError::Network(e.to_string());
                    if !should_retry(&bt_err) || attempts >= MAX_ATTEMPTS {
                        *requests_sent += attempts;
                        return Err(bt_err);
                    }
                    match backoff.next_backoff() {
                        Some(delay) => sleep(delay).await,
                        None => {
                            *requests_sent += attempts;
                            return Err(bt_err);
                        }
                    }
                }
            }
        }
    }

    async fn upload_payload_with_retry(
        &self,
        upload: &crate::types::Logs3OverflowUpload,
        payload_bytes: &[u8],
        requests_sent: &mut usize,
    ) -> Result<()> {
        let mut attempts = 0usize;
        let mut backoff = new_backoff();
        loop {
            attempts += 1;
            let result = upload_overflow_payload(&self.client, upload, payload_bytes).await;
            match result {
                Ok(()) => {
                    *requests_sent += attempts;
                    return Ok(());
                }
                Err(e) => {
                    let bt_err = BraintrustError::Network(e.to_string());
                    if !should_retry(&bt_err) || attempts >= MAX_ATTEMPTS {
                        *requests_sent += attempts;
                        return Err(bt_err);
                    }
                    match backoff.next_backoff() {
                        Some(delay) => sleep(delay).await,
                        None => {
                            *requests_sent += attempts;
                            return Err(bt_err);
                        }
                    }
                }
            }
        }
    }

    async fn post_logs3_raw(&self, payload: &str) -> Result<()> {
        let url = self
            .api_url
            .join("logs3")
            .map_err(|e| BraintrustError::InvalidConfig(format!("invalid logs3 URL: {e}")))?;
        let mut request = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .body(payload.to_string());
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

impl RowPayload {
    fn from_row(row: &Map<String, Value>) -> Result<Self> {
        let row_json = serde_json::to_string(row).map_err(|e| {
            BraintrustError::InvalidConfig(format!("failed to serialize logs3 row: {e}"))
        })?;
        let row_bytes = row_json.len();
        let mut object_ids = serde_json::Map::new();
        for key in OBJECT_ID_KEYS {
            if let Some(value) = row.get(*key) {
                object_ids.insert(key.to_string(), value.clone());
            }
        }
        let is_delete = row.get("_object_delete").and_then(Value::as_bool);
        Ok(Self {
            row_json,
            row_bytes,
            overflow_meta: Logs3OverflowInputRow {
                object_ids,
                is_delete,
                input_row: Logs3OverflowInputRowMeta {
                    byte_size: row_bytes,
                },
            },
        })
    }
}

fn construct_logs3_payload(items: &[RowPayload]) -> String {
    let rows = items
        .iter()
        .map(|item| item.row_json.as_str())
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"rows\":[{rows}],\"api_version\":{LOGS_API_VERSION}}}")
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

fn new_backoff() -> backoff::ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_millis(BASE_BACKOFF_MS))
        .with_multiplier(2.0)
        .with_randomization_factor(0.2)
        .with_max_interval(Duration::from_millis(MAX_BACKOFF_MS))
        .with_max_elapsed_time(None)
        .build()
}

async fn run_with_backoff<T, F, Fut>(mut operation: F) -> (Result<T>, usize)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempts = 0usize;
    let mut backoff = new_backoff();

    loop {
        attempts += 1;
        match operation().await {
            Ok(value) => return (Ok(value), attempts),
            Err(err) => {
                if !should_retry(&err) || attempts >= MAX_ATTEMPTS {
                    return (Err(err), attempts);
                }
                match backoff.next_backoff() {
                    Some(delay) => sleep(delay).await,
                    None => return (Err(err), attempts),
                }
            }
        }
    }
}
