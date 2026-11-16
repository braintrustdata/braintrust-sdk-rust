use super::batching::SerializedBatch;
use super::config::LogQueueConfig;
use crate::internal::upload::{Capabilities, UploadCoordinator, UploadPolicy};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
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

/// Compatibility adapter for the queue; protocol behavior lives in one coordinator.
pub(crate) async fn fetch_version_info(
    client: &reqwest::Client,
    api_url: &Url,
    token: &str,
    org_name: Option<&str>,
) -> (usize, bool) {
    let fallback = crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2;
    match UploadCoordinator::new(
        client.clone(),
        api_url,
        token,
        org_name,
        UploadPolicy::Queue { retries: 0 },
    ) {
        Ok(coordinator) => {
            let capabilities = coordinator.capabilities(fallback, None).await;
            (capabilities.max_request_size, capabilities.overflow)
        }
        Err(error) => {
            warn!(%error, "invalid ingestion scope");
            (fallback, false)
        }
    }
}

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
    save_payload_debug(&batch.data, config.all_publish_payloads_dir(), "all").ok();
    let data = batch.data.clone();
    let coordinator = UploadCoordinator::new(
        client.clone(),
        api_url,
        token,
        org_name,
        UploadPolicy::Queue {
            retries: config.num_retries(),
        },
    )?;
    let result = coordinator
        .upload(
            batch,
            Capabilities {
                max_request_size,
                overflow: can_use_overflow,
            },
        )
        .await;
    if result.is_err() {
        save_payload_debug(&data, config.failed_publish_payloads_dir(), "failed").ok();
    }
    result.map(|_| ()).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_queue::config::LogQueueConfig;

    fn make_batch() -> SerializedBatch {
        SerializedBatch::partition(
            vec![crate::internal::ingest::EncodedRow::from_map(&serde_json::Map::new()).unwrap()],
            1,
            usize::MAX,
        )
        .pop()
        .unwrap()
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
    async fn rejected_direct_batch_splits_in_order_without_retrying_identical_bytes() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                ResponseTemplate::new(if body["rows"].as_array().unwrap().len() > 1 {
                    413
                } else {
                    200
                })
            })
            .expect(5)
            .mount(&server)
            .await;
        let rows = ["first", "second", "third"]
            .into_iter()
            .map(|id| {
                crate::internal::ingest::EncodedRow::from_map(
                    serde_json::json!({"id":id}).as_object().unwrap(),
                )
                .unwrap()
            })
            .collect();
        let batch = SerializedBatch::partition(rows, 100, usize::MAX)
            .pop()
            .unwrap();
        send_batch_with_retry(
            &reqwest::Client::new(),
            &Url::parse(&server.uri()).unwrap(),
            "token",
            None,
            batch,
            &LogQueueConfig::builder().num_retries(2).build(),
            usize::MAX,
            false,
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        let delivered: Vec<_> = requests
            .iter()
            .filter_map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let rows = body["rows"].as_array().unwrap();
                (rows.len() == 1).then(|| rows[0]["id"].clone())
            })
            .collect();
        assert_eq!(
            delivered,
            vec![
                serde_json::json!("first"),
                serde_json::json!("second"),
                serde_json::json!("third")
            ]
        );
    }

    #[tokio::test]
    async fn rejected_singleton_uses_overflow_once_when_supported() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(413))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/logs3/overflow"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"method":"PUT","signedUrl":format!("{}/object",server.uri()),"key":"key"})))
            .expect(1).mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/object"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        send_batch_with_retry(
            &reqwest::Client::new(),
            &Url::parse(&server.uri()).unwrap(),
            "token",
            None,
            make_batch(),
            &LogQueueConfig::builder().num_retries(0).build(),
            usize::MAX,
            true,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn exhausted_queue_retries_send_exactly_the_configured_attempts() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&server)
            .await;
        let result = send_batch_with_retry(
            &reqwest::Client::new(),
            &Url::parse(&server.uri()).unwrap(),
            "token",
            None,
            make_batch(),
            &LogQueueConfig::builder().num_retries(2).build(),
            usize::MAX,
            false,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn overflow_reference_retry_reuses_the_successful_grant_and_object_upload() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3/overflow"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "method":"PUT", "signedUrl":format!("{}/object", server.uri()), "key":"saved-grant"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/object"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        send_batch_with_retry(
            &reqwest::Client::new(),
            &Url::parse(&server.uri()).unwrap(),
            "token",
            None,
            make_batch(),
            &LogQueueConfig::builder().num_retries(1).build(),
            1,
            true,
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 4);
        let object = requests.iter().find(|r| r.url.path() == "/object").unwrap();
        assert!(!object
            .headers
            .keys()
            .any(|name| name.as_str().eq_ignore_ascii_case("authorization")));
        for request in requests.iter().filter(|r| r.url.path() == "/logs3") {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["rows"]["key"], "saved-grant");
        }
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
            make_batch(),
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
            make_batch(),
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
            make_batch(),
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
            make_batch(),
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
            make_batch(),
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
            make_batch(),
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
