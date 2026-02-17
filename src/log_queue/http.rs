use super::batching::SerializedBatch;
use super::config::LogQueueConfig;
use crate::types::{Logs3OverflowReference, Logs3OverflowRequest, Logs3OverflowUpload};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;
use url::Url;

/// Save payload bytes to a debug directory if configured.
pub(crate) fn save_payload_debug(
    data: &[u8],
    dir: Option<PathBuf>,
    prefix: &str,
) -> Result<(), anyhow::Error> {
    if let Some(dir) = dir.as_ref() {
        std::fs::create_dir_all(dir)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let filename = dir.join(format!("{}-{}.json", prefix, timestamp));
        std::fs::write(&filename, data)?;
        tracing::debug!("Saved payload to {}", filename.display());
    }
    Ok(())
}

/// Fetch version info from the server and return (max_request_size, can_use_overflow).
///
/// `can_use_overflow` is true when the server returns a positive `logs3_payload_max_bytes`,
/// indicating that the overflow upload path is supported.
pub(crate) async fn fetch_version_info(
    client: &reqwest::Client,
    api_url: &Url,
    token: &str,
    org_name: Option<&str>,
) -> (usize, bool) {
    let version_url = match api_url.join("version") {
        Ok(url) => url,
        Err(e) => {
            warn!(error = %e, "Invalid API URL for version fetch");
            return (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false);
        }
    };

    let mut request = client.get(version_url).bearer_auth(token);
    if let Some(org) = org_name {
        request = request.header("x-bt-org-name", org);
    }
    let response = request.send().await;

    match response {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(json) => {
                let server_limit = json
                    .get("logs3_payload_max_bytes")
                    .and_then(|v| v.as_u64())
                    .filter(|&v| v > 0)
                    .map(|v| v as usize);

                match server_limit {
                    Some(limit) => (limit, true),
                    None => (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false),
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to parse version response");
                (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false)
            }
        },
        Ok(resp) => {
            warn!(status = %resp.status(), "Version fetch returned non-success status");
            (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false)
        }
        Err(e) => {
            warn!(error = %e, "Failed to fetch version info");
            (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false)
        }
    }
}

/// Send a batch with retry logic (matching TypeScript SDK behavior).
///
/// If the batch exceeds `max_request_size` and `can_use_overflow` is true,
/// uses the overflow upload path (POST logs3/overflow → signed URL → POST logs3).
///
/// Retry semantics match the TypeScript SDK:
/// - `config.num_retries()` is the number of *retries* (not total attempts)
/// - Total attempts = num_retries + 1
/// - Retry delay starts at 1 second, doubles each attempt: `1s * 2^attempt`
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_batch_with_retry(
    client: &reqwest::Client,
    api_url: &Url,
    token: &str,
    org_name: Option<&str>,
    batch: SerializedBatch,
    config: &LogQueueConfig,
    max_request_size: usize,
    can_use_overflow: bool,
) -> Result<(), anyhow::Error> {
    let json_bytes = batch.data;
    let overflow_rows = batch.overflow_rows;

    // Save to debug directories if configured
    save_payload_debug(&json_bytes, config.all_publish_payloads_dir(), "all")?;

    let logs_url = api_url
        .join("logs3")
        .map_err(|e| anyhow::anyhow!("invalid logs url: {e}"))?;

    let use_overflow = can_use_overflow && json_bytes.len() > max_request_size;

    // If overflow is needed, the signed URL is requested once and reused across retries
    // (matches TypeScript SDK's `overflowUpload` caching pattern).
    let mut overflow_upload: Option<Logs3OverflowUpload> = None;

    // Total attempts = num_retries + 1 (matches TypeScript SDK: numTries = env + 1)
    for attempt in 0..=config.num_retries() {
        // Sleep before each retry (exponential backoff: 1s, 2s, 4s, ...)
        if attempt > 0 {
            let delay = Duration::from_secs(
                1u64.checked_shl((attempt - 1) as u32)
                    .unwrap_or(u64::MAX / 2),
            );
            tokio::time::sleep(delay).await;
        }

        if use_overflow {
            // Get or create the overflow upload (cached across retries)
            if overflow_upload.is_none() {
                let upload_result =
                    request_overflow_upload(client, api_url, token, &json_bytes, &overflow_rows)
                        .await;

                match upload_result {
                    Ok(upload) => {
                        match upload_overflow_payload(client, &upload, &json_bytes).await {
                            Ok(()) => overflow_upload = Some(upload),
                            Err(e) => {
                                if attempt < config.num_retries() {
                                    warn!(error = %e, attempt, "overflow upload failed, retrying");
                                    continue;
                                }
                                save_payload_debug(
                                    &json_bytes,
                                    config.failed_publish_payloads_dir(),
                                    "failed",
                                )?;
                                return Err(e);
                            }
                        }
                    }
                    Err(e) => {
                        if attempt < config.num_retries() {
                            warn!(error = %e, attempt, "overflow URL request failed, retrying");
                            continue;
                        }
                        save_payload_debug(
                            &json_bytes,
                            config.failed_publish_payloads_dir(),
                            "failed",
                        )?;
                        return Err(e);
                    }
                }
            }

            // Send the overflow reference to logs3
            let key = overflow_upload.as_ref().unwrap().key.clone();
            let overflow_ref = Logs3OverflowRequest {
                rows: Logs3OverflowReference {
                    reference_type: "logs3_overflow",
                    key,
                },
                api_version: crate::types::LOGS_API_VERSION,
            };
            let overflow_bytes = serde_json::to_vec(&overflow_ref)
                .map_err(|e| anyhow::anyhow!("failed to serialize overflow ref: {e}"))?;

            let mut overflow_req = client
                .post(logs_url.clone())
                .bearer_auth(token)
                .header("content-type", "application/json")
                .body(overflow_bytes);
            if let Some(org) = org_name {
                overflow_req = overflow_req.header("x-bt-org-name", org);
            }
            match overflow_req.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    if attempt < config.num_retries() {
                        warn!(status = %status, attempt, "overflow ref send failed, retrying");
                        continue;
                    }
                    save_payload_debug(
                        &json_bytes,
                        config.failed_publish_payloads_dir(),
                        "failed",
                    )?;
                    anyhow::bail!("overflow ref send failed: [{status}] {body}");
                }
                Err(e) => {
                    if attempt < config.num_retries() {
                        warn!(error = %e, attempt, "overflow ref send network error, retrying");
                        continue;
                    }
                    save_payload_debug(
                        &json_bytes,
                        config.failed_publish_payloads_dir(),
                        "failed",
                    )?;
                    return Err(e.into());
                }
            }
        } else {
            // Normal path: POST directly to logs3
            let mut normal_req = client
                .post(logs_url.clone())
                .bearer_auth(token)
                .header("content-type", "application/json")
                .body(json_bytes.clone());
            if let Some(org) = org_name {
                normal_req = normal_req.header("x-bt-org-name", org);
            }
            match normal_req.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let is_retryable = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || status.is_server_error();
                    if is_retryable && attempt < config.num_retries() {
                        warn!(status = %status, attempt, "batch send failed with retryable error, retrying");
                        continue;
                    }
                    save_payload_debug(
                        &json_bytes,
                        config.failed_publish_payloads_dir(),
                        "failed",
                    )?;
                    anyhow::bail!("batch send failed: [{status}] {body}");
                }
                Err(e) => {
                    if attempt < config.num_retries() {
                        warn!(error = %e, attempt, "batch send network error, retrying");
                        continue;
                    }
                    save_payload_debug(
                        &json_bytes,
                        config.failed_publish_payloads_dir(),
                        "failed",
                    )?;
                    return Err(e.into());
                }
            }
        }
    }

    unreachable!("retry loop should always return or continue")
}

/// Request an overflow upload URL from the server.
pub(crate) async fn request_overflow_upload(
    client: &reqwest::Client,
    api_url: &Url,
    token: &str,
    json_bytes: &[u8],
    overflow_rows: &[crate::types::Logs3OverflowInputRow],
) -> Result<Logs3OverflowUpload, anyhow::Error> {
    let overflow_url = api_url
        .join("logs3/overflow")
        .map_err(|e| anyhow::anyhow!("invalid overflow url: {e}"))?;

    let request_body = serde_json::json!({
        "content_type": "application/json",
        "size_bytes": json_bytes.len(),
        "rows": overflow_rows,
    });

    let resp = client
        .post(overflow_url)
        .bearer_auth(token)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("overflow URL request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("overflow URL request failed: [{status}] {body}");
    }

    let upload: Logs3OverflowUpload = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("failed to parse overflow response: {e}"))?;

    Ok(upload)
}

/// Upload the payload to the signed URL provided by the overflow response.
pub(crate) async fn upload_overflow_payload(
    client: &reqwest::Client,
    upload: &Logs3OverflowUpload,
    payload: &[u8],
) -> Result<(), anyhow::Error> {
    let resp = if upload.method.eq_ignore_ascii_case("POST") {
        // Multipart POST (e.g. S3)
        let fields = upload
            .fields
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("overflow POST upload missing fields"))?;

        let mut form = reqwest::multipart::Form::new();
        for (key, value) in fields {
            form = form.text(key.clone(), value.clone());
        }
        let content_type = fields
            .get("Content-Type")
            .map(|s| s.as_str())
            .unwrap_or("application/json");
        let part = reqwest::multipart::Part::bytes(payload.to_vec())
            .mime_str(content_type)
            .map_err(|e| anyhow::anyhow!("invalid content-type: {e}"))?;
        form = form.part("file", part);

        let mut req = client.post(&upload.signed_url).multipart(form);
        if let Some(headers) = &upload.headers {
            for (key, value) in headers {
                if key.to_lowercase() != "content-type" {
                    req = req.header(key, value);
                }
            }
        }
        req.send().await
    } else {
        // PUT (e.g. Azure, GCS)
        let mut req = client
            .put(&upload.signed_url)
            .header("content-type", "application/json")
            .body(payload.to_vec());
        if let Some(headers) = &upload.headers {
            for (key, value) in headers {
                req = req.header(key, value);
            }
        }
        req.send().await
    };

    let resp = resp.map_err(|e| anyhow::anyhow!("overflow upload failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("overflow upload failed: [{status}] {body}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_queue::config::LogQueueConfig;

    fn make_batch(data: &[u8]) -> SerializedBatch {
        SerializedBatch {
            data: data.to_vec(),
            overflow_rows: vec![],
        }
    }

    #[test]
    fn test_save_payload_debug() {
        use std::fs;
        let temp_dir = std::env::temp_dir().join("braintrust_test_payloads");
        fs::create_dir_all(&temp_dir).ok();

        let data = b"{\"test\": \"payload\"}";

        // Test saving
        save_payload_debug(data, Some(temp_dir.clone()), "test").unwrap();

        // Verify file was created
        let files: Vec<_> = fs::read_dir(&temp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("test-"))
            .collect();

        assert!(!files.is_empty(), "Should create debug file");

        // Verify content
        let content = fs::read(files[0].path()).unwrap();
        assert_eq!(content, data);

        // Cleanup
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_save_payload_debug_none() {
        // Should not fail when dir is None
        let result = save_payload_debug(b"test", None, "prefix");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_batch_with_retry_success() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let api_url = Url::parse(&server.uri()).unwrap();
        let config = LogQueueConfig::default();

        let result = send_batch_with_retry(
            &client,
            &api_url,
            "token",
            None,
            make_batch(b"{}"),
            &config,
            usize::MAX,
            false,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_batch_with_retry_5xx() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // First two attempts fail with 500, third succeeds
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let api_url = Url::parse(&server.uri()).unwrap();
        // 2 retries = 3 total attempts
        let config = LogQueueConfig::builder().num_retries(2).build();

        let result = send_batch_with_retry(
            &client,
            &api_url,
            "token",
            None,
            make_batch(b"{}"),
            &config,
            usize::MAX,
            false,
        )
        .await;

        assert!(result.is_ok(), "Should succeed after retries");
    }

    #[tokio::test]
    async fn test_send_batch_with_retry_429() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Rate limit, then success
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let api_url = Url::parse(&server.uri()).unwrap();
        let config = LogQueueConfig::default();

        let result = send_batch_with_retry(
            &client,
            &api_url,
            "token",
            None,
            make_batch(b"{}"),
            &config,
            usize::MAX,
            false,
        )
        .await;

        assert!(result.is_ok(), "Should retry on 429");
    }

    #[tokio::test]
    async fn test_send_batch_with_retry_4xx_no_retry() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let api_url = Url::parse(&server.uri()).unwrap();
        let config = LogQueueConfig::default();

        let result = send_batch_with_retry(
            &client,
            &api_url,
            "token",
            None,
            make_batch(b"{}"),
            &config,
            usize::MAX,
            false,
        )
        .await;

        assert!(result.is_err(), "Should not retry on 400");
    }

    #[tokio::test]
    async fn test_send_batch_with_retry_max_retries() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let api_url = Url::parse(&server.uri()).unwrap();
        // 1 retry = 2 total attempts
        let config = LogQueueConfig::builder().num_retries(1).build();

        let result = send_batch_with_retry(
            &client,
            &api_url,
            "token",
            None,
            make_batch(b"{}"),
            &config,
            usize::MAX,
            false,
        )
        .await;

        assert!(result.is_err(), "Should fail after max retries");
    }

    #[tokio::test]
    async fn test_num_retries_is_total_retries_not_attempts() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

        // With num_retries=3, we should make 4 total attempts (initial + 3 retries)
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(3) // First 3 attempts fail
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let api_url = Url::parse(&server.uri()).unwrap();
        let config = LogQueueConfig::builder().num_retries(3).build();

        let result = send_batch_with_retry(
            &client,
            &api_url,
            "token",
            None,
            make_batch(b"{}"),
            &config,
            usize::MAX,
            false,
        )
        .await;

        assert!(
            result.is_ok(),
            "4th attempt should succeed with num_retries=3"
        );
    }
}
