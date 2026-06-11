use std::time::Duration;

use backoff::backoff::Backoff;
use backoff::ExponentialBackoffBuilder;
use serde_json::{json, Map, Value};
use tokio::time::sleep;

use crate::api::logs3::{
    Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3OverflowUpload,
    Logs3OverflowUploadRequest, LOGS_API_VERSION, OBJECT_ID_KEYS,
};
use crate::api::ApiClient;
use crate::api::LoginState;
use crate::error::{BraintrustError, Result};
use crate::log_queue::batch_items;

const DEFAULT_MAX_REQUEST_SIZE: usize = 6 * 1024 * 1024;
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
    api: ApiClient,
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
        let api_url = api_url.as_ref().to_string();
        let state = LoginState::new();
        let _ = state.set(
            api_key.into(),
            String::new(),
            org_name.unwrap_or_default(),
            api_url.clone(),
            api_url,
        );
        Ok(Self::from_api_client_internal(ApiClient::from_login_state(
            state,
        )?))
    }

    #[cfg(feature = "internal-api")]
    pub fn from_api_client(api: ApiClient) -> Self {
        Self::from_api_client_internal(api)
    }

    fn from_api_client_internal(api: ApiClient) -> Self {
        Self {
            api,
            max_request_size_override: None,
            payload_limits: None,
        }
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
        let mut server_limit: Option<usize> = None;
        if let Ok(version) = self.api.logs3().version().await {
            server_limit = version.logs3_payload_max_bytes().filter(|v| *v > 0);
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
        let (upload_result, upload_attempts) =
            run_with_backoff(|| self.request_overflow_upload(&overflow_rows, payload_bytes)).await;
        requests_sent += upload_attempts;
        let upload = upload_result?;

        let (upload_payload_result, upload_payload_attempts) =
            run_with_backoff(|| self.upload_overflow_payload(&upload, &payload)).await;
        requests_sent += upload_payload_attempts;
        upload_payload_result?;

        let overflow_reference = json!({
            "rows": {
                "type": LOGS3_OVERFLOW_REFERENCE_TYPE,
                "key": upload.key(),
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

    async fn request_overflow_upload(
        &self,
        rows: &[Logs3OverflowInputRow],
        payload_bytes: usize,
    ) -> Result<Logs3OverflowUpload> {
        self.api
            .logs3()
            .request_overflow_upload(Logs3OverflowUploadRequest {
                content_type: "application/json".to_string(),
                size_bytes: payload_bytes,
                rows: rows.to_vec(),
            })
            .await
    }

    async fn upload_overflow_payload(
        &self,
        upload: &Logs3OverflowUpload,
        payload: &str,
    ) -> Result<()> {
        self.api
            .upload_signed_payload(upload, payload.as_bytes())
            .await
    }

    async fn post_logs3_raw(&self, payload: &str) -> Result<()> {
        self.api.logs3().post_raw(payload.as_bytes().to_vec()).await
    }
}

impl RowPayload {
    fn from_row(row: &Map<String, Value>) -> Result<Self> {
        let row_json = serde_json::to_string(row).map_err(|e| {
            BraintrustError::InvalidConfig(format!("failed to serialize logs3 row: {e}"))
        })?;
        let row_bytes = row_json.len();
        let mut object_ids = Map::new();
        for &key in OBJECT_ID_KEYS {
            if let Some(value) = row.get(key) {
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
    let mut attempts = 0usize;
    let mut backoff = ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_millis(BASE_BACKOFF_MS))
        .with_multiplier(2.0)
        .with_randomization_factor(0.2)
        .with_max_interval(Duration::from_millis(MAX_BACKOFF_MS))
        .with_max_elapsed_time(None)
        .build();

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
