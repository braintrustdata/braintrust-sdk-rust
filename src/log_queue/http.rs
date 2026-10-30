use super::batching::SerializedBatch;
use super::config::LogQueueConfig;
use crate::api::transport::{Logs3OverflowRequest, Logs3OverflowUpload};
use crate::error::BraintrustError;
use crate::http::{api_client, api_result};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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
        let unique_id = uuid::Uuid::new_v4();
        let filename = dir.join(format!("{}-{}-{}.json", prefix, timestamp, unique_id));
        std::fs::write(&filename, data)?;
        tracing::debug!("Saved payload to {}", filename.display());
    }
    Ok(())
}

/// Fetch logging capabilities, falling back to conservative defaults when unavailable.
pub(crate) async fn fetch_version_info(
    client: &reqwest::Client,
    api_url: &Url,
    token: &str,
    org_name: Option<&str>,
) -> (usize, bool) {
    let result = async {
        let api = api_client(
            client,
            api_url.as_str(),
            crate::api::DEFAULT_APP_URL,
            token,
            org_name,
        )?;
        api_result(api.version().await).await
    }
    .await;
    match result {
        Ok(version) => match version.logs3_payload_max_bytes.filter(|limit| *limit > 0) {
            Some(limit) => (limit, true),
            None => (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false),
        },
        Err(error) => {
            warn!(%error, "Failed to fetch version info");
            (crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2, false)
        }
    }
}

/// Deliver a serialized batch, preserving retry and staged overflow semantics.
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
    save_payload_debug(&json_bytes, config.all_publish_payloads_dir(), "all").ok();
    let api = api_client(
        client,
        api_url.as_str(),
        crate::api::DEFAULT_APP_URL,
        token,
        org_name,
    )?;
    let use_overflow = can_use_overflow && json_bytes.len() > max_request_size;
    let mut overflow_upload: Option<Logs3OverflowUpload> = None;
    for attempt in 0..=config.num_retries() {
        if attempt > 0 {
            let delay_secs = 1u64.checked_shl((attempt - 1) as u32).unwrap_or(30).min(30);
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        if use_overflow && overflow_upload.is_none() {
            let upload = async {
                let upload = api_result(
                    api.logs3_overflow(&Logs3OverflowRequest {
                        rows: batch.overflow_rows.clone(),
                        content_type: Some("application/json".to_owned()),
                    })
                    .await,
                )
                .await?
                .into_inner();
                api_result(api.upload_logs3_overflow(&upload, json_bytes.clone()).await).await?;
                Ok::<_, BraintrustError>(upload)
            }
            .await;
            match upload {
                Ok(upload) => overflow_upload = Some(upload),
                Err(error) => {
                    if attempt < config.num_retries() {
                        warn!(%error, attempt, "overflow upload failed, retrying");
                        continue;
                    }
                    save_payload_debug(&json_bytes, config.failed_publish_payloads_dir(), "failed")
                        .ok();
                    return Err(error.into());
                }
            }
        }
        let payload = match &overflow_upload {
            Some(upload) => serde_json::to_vec(&serde_json::json!({
                "rows": upload.reference(),
                "api_version": crate::types::LOGS_API_VERSION,
            }))?,
            None => json_bytes.clone(),
        };
        let started = Instant::now();
        match api_result(api.logs3_raw(payload).await).await {
            Ok(_) => {
                tracing::debug!(
                    bytes = json_bytes.len(),
                    elapsed_ms = started.elapsed().as_millis(),
                    "batch sent"
                );
                return Ok(());
            }
            Err(error) => {
                let retryable = match &error {
                    BraintrustError::Api { status, .. } => {
                        *status == 429 || (500..=599).contains(status)
                    }
                    BraintrustError::Http(_) => true,
                    _ => false,
                };
                if retryable && attempt < config.num_retries() {
                    warn!(%error, attempt, "batch send failed, retrying");
                    continue;
                }
                save_payload_debug(&json_bytes, config.failed_publish_payloads_dir(), "failed")
                    .ok();
                return Err(error.into());
            }
        }
    }
    unreachable!("retry loop always returns")
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
