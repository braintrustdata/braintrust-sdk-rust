use super::batching::SerializedBatch;
use super::config::LogQueueConfig;
use crate::api::logs3::{
    Logs3OverflowInputRow, Logs3OverflowReference, Logs3OverflowRequest, Logs3OverflowUpload,
    Logs3OverflowUploadRequest, LOGS_API_VERSION,
};
use crate::api::ApiClient;
use crate::error::BraintrustError;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

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
        let unique_id = uuid::Uuid::new_v4();
        let filename = dir.join(format!("{}-{}-{}.json", prefix, timestamp, unique_id));
        std::fs::write(&filename, data)?;
        tracing::debug!("Saved payload to {}", filename.display());
    }
    Ok(())
}

/// Fetch version info from the server and return (max_request_size, can_use_overflow).
///
/// `can_use_overflow` is true when the server returns a positive `logs3_payload_max_bytes`,
/// indicating that the overflow upload path is supported.
pub(crate) async fn fetch_version_info(api: &ApiClient) -> (usize, bool) {
    match api.logs3().version().await {
        Ok(version) => match version.logs3_payload_max_bytes().filter(|&v| v > 0) {
            Some(limit) => (limit, true),
            None => (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false),
        },
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
///
/// Note: the TypeScript SDK retries on all errors including 4xx. This implementation
/// intentionally diverges: only 429 and 5xx are retried since retrying on 400 Bad
/// Request (malformed payload) cannot succeed and wastes time.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_batch_with_retry(
    api: &ApiClient,
    batch: SerializedBatch,
    config: &LogQueueConfig,
    max_request_size: usize,
    can_use_overflow: bool,
) -> Result<(), anyhow::Error> {
    let json_bytes = batch.data;
    let overflow_rows = batch.overflow_rows;

    // Save to debug directories if configured
    save_payload_debug(&json_bytes, config.all_publish_payloads_dir(), "all").ok();

    let use_overflow = can_use_overflow && json_bytes.len() > max_request_size;

    // If overflow is needed, the signed URL is requested once and reused across retries
    // (matches TypeScript SDK's `overflowUpload` caching pattern).
    let mut overflow_upload: Option<Logs3OverflowUpload> = None;

    // Total attempts = num_retries + 1 (matches TypeScript SDK: numTries = env + 1)
    for attempt in 0..=config.num_retries() {
        // Sleep before each retry (exponential backoff: 1s, 2s, 4s, ..., capped at 30s)
        if attempt > 0 {
            let delay_secs = 1u64.checked_shl((attempt - 1) as u32).unwrap_or(30).min(30);
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }

        if use_overflow {
            // Get or create the overflow upload (cached across retries)
            if overflow_upload.is_none() {
                let upload_result = request_overflow_upload(api, &json_bytes, &overflow_rows).await;

                match upload_result {
                    Ok(upload) => match upload_overflow_payload(api, &upload, &json_bytes).await {
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
                            )
                            .ok();
                            return Err(e);
                        }
                    },
                    Err(e) => {
                        if attempt < config.num_retries() {
                            warn!(error = %e, attempt, "overflow URL request failed, retrying");
                            continue;
                        }
                        save_payload_debug(
                            &json_bytes,
                            config.failed_publish_payloads_dir(),
                            "failed",
                        )
                        .ok();
                        return Err(e);
                    }
                }
            }

            // Send the overflow reference to logs3
            let key = overflow_upload.as_ref().unwrap().key().to_string();
            let overflow_ref = Logs3OverflowRequest {
                rows: Logs3OverflowReference {
                    reference_type: "logs3_overflow".to_string(),
                    key,
                },
                api_version: LOGS_API_VERSION,
            };
            let overflow_bytes = serde_json::to_vec(&overflow_ref)
                .map_err(|e| anyhow::anyhow!("failed to serialize overflow ref: {e}"))?;

            let req_start = Instant::now();
            match api.logs3().post_raw(overflow_bytes).await {
                Ok(()) => {
                    tracing::debug!(
                        bytes = json_bytes.len(),
                        elapsed_ms = req_start.elapsed().as_millis(),
                        "overflow batch sent"
                    );
                    return Ok(());
                }
                Err(e) => {
                    if is_retryable_api_error(&e) && attempt < config.num_retries() {
                        warn!(error = %e, attempt, "overflow ref send failed, retrying");
                        continue;
                    }
                    save_payload_debug(&json_bytes, config.failed_publish_payloads_dir(), "failed")
                        .ok();
                    return Err(anyhow::anyhow!("overflow ref send failed: {e}"));
                }
            }
        } else {
            // Normal path: POST directly to logs3
            let req_start = Instant::now();
            match api.logs3().post_raw(json_bytes.clone()).await {
                Ok(()) => {
                    tracing::debug!(
                        bytes = json_bytes.len(),
                        elapsed_ms = req_start.elapsed().as_millis(),
                        "batch sent"
                    );
                    return Ok(());
                }
                Err(e) => {
                    if is_retryable_api_error(&e) && attempt < config.num_retries() {
                        warn!(error = %e, attempt, "batch send failed with retryable error, retrying");
                        continue;
                    }
                    save_payload_debug(&json_bytes, config.failed_publish_payloads_dir(), "failed")
                        .ok();
                    return Err(anyhow::anyhow!("batch send failed: {e}"));
                }
            }
        }
    }

    unreachable!("retry loop should always return or continue")
}

/// Request an overflow upload URL from the server.
pub(crate) async fn request_overflow_upload(
    api: &ApiClient,
    json_bytes: &[u8],
    overflow_rows: &[Logs3OverflowInputRow],
) -> Result<Logs3OverflowUpload, anyhow::Error> {
    api.logs3()
        .request_overflow_upload(Logs3OverflowUploadRequest {
            content_type: "application/json".to_string(),
            size_bytes: json_bytes.len(),
            rows: overflow_rows.to_vec(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("overflow URL request failed: {e}"))
}

fn is_retryable_api_error(error: &BraintrustError) -> bool {
    match error {
        BraintrustError::Network(_)
        | BraintrustError::ChannelClosed
        | BraintrustError::Background(_) => true,
        BraintrustError::Api { status, .. } => *status == 429 || (*status >= 500 && *status <= 599),
        BraintrustError::InvalidConfig(_) | BraintrustError::StreamAggregation(_) => false,
    }
}

/// Upload the payload to the signed URL provided by the overflow response.
pub(crate) async fn upload_overflow_payload(
    api: &ApiClient,
    upload: &Logs3OverflowUpload,
    payload: &[u8],
) -> Result<(), anyhow::Error> {
    api.upload_signed_payload(upload, payload)
        .await
        .map_err(|err| anyhow::anyhow!("overflow upload failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::LoginState;
    use crate::log_queue::config::LogQueueConfig;

    fn make_batch(data: &[u8]) -> SerializedBatch {
        SerializedBatch {
            data: data.to_vec(),
            overflow_rows: vec![],
        }
    }

    fn api_client(url: String) -> ApiClient {
        let state = LoginState::new();
        assert!(state.set(
            "token".to_string(),
            "org-id".to_string(),
            String::new(),
            url.clone(),
            url,
        ));
        ApiClient::from_login_state(state).expect("api client")
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

        let api = api_client(server.uri());
        let config = LogQueueConfig::default();

        let result =
            send_batch_with_retry(&api, make_batch(b"{}"), &config, usize::MAX, false).await;

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

        let api = api_client(server.uri());
        // 2 retries = 3 total attempts
        let config = LogQueueConfig::builder().num_retries(2).build();

        let result =
            send_batch_with_retry(&api, make_batch(b"{}"), &config, usize::MAX, false).await;

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

        let api = api_client(server.uri());
        let config = LogQueueConfig::default();

        let result =
            send_batch_with_retry(&api, make_batch(b"{}"), &config, usize::MAX, false).await;

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

        let api = api_client(server.uri());
        let config = LogQueueConfig::default();

        let result =
            send_batch_with_retry(&api, make_batch(b"{}"), &config, usize::MAX, false).await;

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

        let api = api_client(server.uri());
        // 1 retry = 2 total attempts
        let config = LogQueueConfig::builder().num_retries(1).build();

        let result =
            send_batch_with_retry(&api, make_batch(b"{}"), &config, usize::MAX, false).await;

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

        let api = api_client(server.uri());
        let config = LogQueueConfig::builder().num_retries(3).build();

        let result =
            send_batch_with_retry(&api, make_batch(b"{}"), &config, usize::MAX, false).await;

        assert!(
            result.is_ok(),
            "4th attempt should succeed with num_retries=3"
        );
    }
}
