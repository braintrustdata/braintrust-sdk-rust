use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{multipart, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::time::sleep;

use crate::error::{BraintrustError, Result};

const DEFAULT_MAX_REQUEST_SIZE: usize = 6 * 1024 * 1024;
const LOGS_API_VERSION: u8 = 2;
const LOGS3_OVERFLOW_REFERENCE_TYPE: &str = "logs3_overflow";
const MAX_ATTEMPTS: usize = 5;
const BASE_BACKOFF_MS: u64 = 300;
const MAX_BACKOFF_MS: u64 = 8_000;
const OBJECT_ID_KEYS: [&str; 6] = [
    "experiment_id",
    "dataset_id",
    "prompt_session_id",
    "project_id",
    "log_id",
    "function_data",
];

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

#[derive(Debug, Clone, Serialize)]
struct Logs3OverflowInputRow {
    object_ids: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_delete: Option<bool>,
    input_row: OverflowInputRowInfo,
}

#[derive(Debug, Clone, Serialize)]
struct OverflowInputRowInfo {
    byte_size: usize,
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
        let batches = batch_items(&row_payloads, batch_limit_rows, batch_limit_bytes);

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
            let payload_bytes = payload.as_bytes().len();

            if limits.can_use_overflow && payload_bytes > limits.max_request_size {
                let overflow_result = self.submit_overflow(&chunk, payload, payload_bytes).await?;
                result.rows_uploaded += overflow_result.rows_uploaded;
                result.bytes_processed += overflow_result.bytes_processed;
                result.requests_sent += overflow_result.requests_sent;
                continue;
            }

            let mut chunk_done = false;
            for attempt in 0..MAX_ATTEMPTS {
                result.requests_sent += 1;
                match self.post_logs3_raw(&payload).await {
                    Ok(()) => {
                        result.rows_uploaded += chunk.len();
                        result.bytes_processed += payload_bytes;
                        chunk_done = true;
                        break;
                    }
                    Err(BraintrustError::Api { status, .. }) if status == 413 => {
                        if limits.can_use_overflow {
                            let overflow_result = self
                                .submit_overflow(&chunk, payload.clone(), payload_bytes)
                                .await?;
                            result.rows_uploaded += overflow_result.rows_uploaded;
                            result.bytes_processed += overflow_result.bytes_processed;
                            result.requests_sent += overflow_result.requests_sent;
                            chunk_done = true;
                            break;
                        }
                        if chunk.len() > 1 {
                            let mid = chunk.len() / 2;
                            pending.push(chunk[mid..].to_vec());
                            pending.push(chunk[..mid].to_vec());
                            chunk_done = true;
                            break;
                        }
                        return Err(BraintrustError::Api {
                            status,
                            message:
                                "single row exceeds server payload limit and overflow is unavailable"
                                    .to_string(),
                        });
                    }
                    Err(err) if should_retry(&err) && attempt + 1 < MAX_ATTEMPTS => {
                        sleep(backoff_delay(attempt)).await;
                    }
                    Err(err) => return Err(err),
                }
            }

            if !chunk_done {
                return Err(BraintrustError::InvalidConfig(
                    "unexpected retry exhaustion in logs3 uploader".to_string(),
                ));
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
        let upload = {
            let mut last_error: Option<BraintrustError> = None;
            let mut upload = None;
            for attempt in 0..MAX_ATTEMPTS {
                requests_sent += 1;
                match self
                    .request_overflow_upload(&overflow_rows, payload_bytes)
                    .await
                {
                    Ok(response) => {
                        upload = Some(response);
                        break;
                    }
                    Err(err) if should_retry(&err) && attempt + 1 < MAX_ATTEMPTS => {
                        last_error = Some(err);
                        sleep(backoff_delay(attempt)).await;
                    }
                    Err(err) => return Err(err),
                }
            }
            upload.ok_or_else(|| {
                last_error.unwrap_or_else(|| {
                    BraintrustError::InvalidConfig("missing overflow upload response".to_string())
                })
            })?
        };

        {
            let mut last_error: Option<BraintrustError> = None;
            let mut uploaded = false;
            for attempt in 0..MAX_ATTEMPTS {
                requests_sent += 1;
                match self.upload_overflow_payload(&upload, &payload).await {
                    Ok(()) => {
                        uploaded = true;
                        break;
                    }
                    Err(err) if should_retry(&err) && attempt + 1 < MAX_ATTEMPTS => {
                        last_error = Some(err);
                        sleep(backoff_delay(attempt)).await;
                    }
                    Err(err) => return Err(err),
                }
            }
            if !uploaded {
                return Err(last_error.unwrap_or_else(|| {
                    BraintrustError::InvalidConfig("logs3 overflow upload failed".to_string())
                }));
            }
        }

        let overflow_reference = json!({
            "rows": {
                "type": LOGS3_OVERFLOW_REFERENCE_TYPE,
                "key": upload.key,
            },
            "api_version": LOGS_API_VERSION,
        })
        .to_string();
        for attempt in 0..MAX_ATTEMPTS {
            requests_sent += 1;
            match self.post_logs3_raw(&overflow_reference).await {
                Ok(()) => {
                    return Ok(Logs3UploadResult {
                        rows_uploaded: items.len(),
                        bytes_processed: payload_bytes,
                        requests_sent,
                    });
                }
                Err(err) if should_retry(&err) && attempt + 1 < MAX_ATTEMPTS => {
                    sleep(backoff_delay(attempt)).await;
                }
                Err(err) => return Err(err),
            }
        }

        Err(BraintrustError::InvalidConfig(
            "unexpected retry exhaustion for overflow logs3 post".to_string(),
        ))
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
        payload: &str,
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
                let file_part = multipart::Part::text(payload.to_string())
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

                let mut request = self.client.put(signed_url).body(payload.to_string());
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
        let row_bytes = row_json.as_bytes().len();
        let mut object_ids = Map::new();
        for key in OBJECT_ID_KEYS {
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
                input_row: OverflowInputRowInfo {
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

fn batch_items(
    items: &[RowPayload],
    batch_max_num_items: usize,
    batch_max_num_bytes: usize,
) -> Vec<Vec<RowPayload>> {
    let max_items = batch_max_num_items.max(1);
    let max_bytes = batch_max_num_bytes.max(1);
    let mut output: Vec<Vec<RowPayload>> = Vec::new();
    let mut batch: Vec<RowPayload> = Vec::new();
    let mut batch_len = 0usize;

    for item in items {
        if !batch.is_empty()
            && (batch.len() >= max_items || batch_len + item.row_bytes >= max_bytes)
        {
            output.push(batch);
            batch = Vec::new();
            batch_len = 0;
        }
        batch_len += item.row_bytes;
        batch.push(item.clone());
    }
    if !batch.is_empty() {
        output.push(batch);
    }
    output
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

fn backoff_delay(attempt: usize) -> Duration {
    let factor = 2u64.saturating_pow(attempt as u32);
    let millis = (BASE_BACKOFF_MS.saturating_mul(factor)).min(MAX_BACKOFF_MS);
    Duration::from_millis(millis.max(BASE_BACKOFF_MS))
}
