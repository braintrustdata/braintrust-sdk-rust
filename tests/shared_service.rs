mod support;

use support::MockServer;

use braintrust_sdk_rust::{BraintrustClient, TokenProvider};
use serde_json::{json, Map, Value};
use std::future::IntoFuture;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::{
    matchers::{body_string_contains, method, path},
    Mock, ResponseTemplate,
};

fn row(id: &str, project: &str) -> Map<String, Value> {
    json!({"id":id,"project_id":project,"log_id":"g"})
        .as_object()
        .unwrap()
        .clone()
}
fn client(server: &MockServer) -> BraintrustClient {
    BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .checked_delivery(true)
        .build_without_login()
        .unwrap()
}
async fn accept(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn construction_and_submission_need_no_caller_runtime() {
    let server = MockServer::start().await;
    accept(&server).await;
    let endpoint = server.uri();
    let client = std::thread::spawn(move || {
        let client = BraintrustClient::builder()
            .api_url(&endpoint)
            .app_url(&endpoint)
            .build_without_login()
            .unwrap();
        client.log_rows_with_credentials("token", "org", None, [row("row", "project")]);
        client
    })
    .join()
    .unwrap();
    client.flush().await.unwrap();
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path() == "/logs3"));
}

#[tokio::test]
async fn delivery_survives_the_constructing_runtime() {
    let server = MockServer::start().await;
    accept(&server).await;
    let endpoint = server.uri();
    let client = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            BraintrustClient::builder()
                .api_url(&endpoint)
                .app_url(&endpoint)
                .checked_delivery(true)
                .skip_login(true)
                .build()
                .await
                .unwrap()
        })
    })
    .join()
    .unwrap();
    client.log_rows_with_credentials("token", "org", None, [row("row", "project")]);
    client.flush().await.unwrap();
}

#[tokio::test]
async fn shutdown_drains_accepted_writes_even_if_its_completion_is_dropped() {
    use braintrust_sdk_rust::{EnqueueError, FlushError};
    let server = MockServer::start().await;
    accept(&server).await;
    let client = client(&server);
    let span = client
        .span_builder_with_credentials("token", "org")
        .parent_info(braintrust_sdk_rust::ParentSpanInfo::ProjectLogs {
            object_id: "p".into(),
        })
        .build();
    let write = span.log(Default::default());
    drop(client.shutdown(Duration::from_secs(2)));
    write.await.unwrap();
    assert!(matches!(
        client.flush().submission(),
        Err(FlushError::Admission(EnqueueError::Closed))
    ));
    // A separate client sharing this service still accepts work.
    let other = self::client(&server);
    other.log_rows_with_credentials("token", "org", None, [row("other", "p")]);
    other.flush().await.unwrap();
}

#[tokio::test]
async fn terminal_failure_is_scoped_and_does_not_poison_later_flushes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("bad"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .mount(&server)
        .await;
    accept(&server).await;
    let bad = client(&server);
    let good = client(&server);
    bad.log_rows_with_credentials("token", "org", None, [row("bad", "bad")]);
    good.log_rows_with_credentials("token", "org", None, [row("good", "good")]);
    assert!(bad.flush().await.is_err());
    good.flush().await.unwrap();
    assert_eq!(bad.dropped_rows().upload_failed, 1);
    assert_eq!(good.dropped_rows().upload_failed, 0);
    bad.flush().await.unwrap();
}

#[tokio::test]
async fn retry_sleep_releases_capacity_for_healthy_orgs() {
    let server = MockServer::start().await;
    let healthy_at = Arc::new(Mutex::new(None));
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("throttled"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .with_priority(1)
        .mount(&server)
        .await;
    let observed = healthy_at.clone();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(move |_: &wiremock::Request| {
            *observed.lock().unwrap() = Some(Instant::now());
            ResponseTemplate::new(200).set_body_string("{}")
        })
        .with_priority(2)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_url(server.uri())
        .app_url(server.uri())
        .with_batch_max_items(1)
        .checked_delivery(true)
        .build_without_login()
        .unwrap();
    // A different org must be able to send while this one retries.
    client.log_rows_with_credentials(
        "token",
        "throttled-org",
        None,
        (0..8).map(|i| row(&i.to_string(), "throttled")),
    );
    client.log_rows_with_credentials("token", "healthy-org", None, [row("healthy", "healthy")]);
    let start = Instant::now();
    assert!(client.flush().await.is_err());
    assert!(
        healthy_at.lock().unwrap().unwrap().duration_since(start) < Duration::from_millis(900),
        "healthy work waited for retry sleep"
    );
}

#[tokio::test]
async fn a_flush_excludes_submissions_after_its_cutoff() {
    let server = MockServer::start().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let observe = started.clone();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(move |_: &wiremock::Request| {
            observe.notify_one();
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(Duration::from_millis(100))
        })
        .mount(&server)
        .await;
    let client = client(&server);
    client.log_rows_with_credentials("token", "org", None, [row("first", "p")]);
    let flush = client.flush().into_future();
    tokio::pin!(flush);
    tokio::select! { _ = &mut flush => panic!("request should still be pending"), _ = started.notified() => {} }
    client.log_rows_with_credentials("token", "org", None, [row("later", "p")]);
    flush.await.unwrap();
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .count(),
        1
    );
    client.flush().await.unwrap();
}

/// Waits until released, then fails every request, including retries.
struct Blocked(Arc<tokio::sync::Semaphore>);
#[async_trait::async_trait]
impl TokenProvider for Blocked {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let _ = self.0.acquire().await;
        Err("unavailable".into())
    }
}
#[tokio::test]
async fn stalled_credentials_and_cancelled_waiters_do_not_block_other_clients() {
    let server = MockServer::start().await;
    accept(&server).await;
    let blocked = client(&server);
    let healthy = client(&server);
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    blocked.log_rows_with_token_provider(
        Arc::new(Blocked(release.clone())),
        "org",
        None,
        [row("blocked", "p")],
    );
    let flush = blocked.flush().into_future();
    tokio::pin!(flush);
    tokio::select! { _ = &mut flush => panic!("provider should wait"), _ = tokio::task::yield_now() => {} }
    healthy.log_rows_with_credentials("token", "org", None, [row("healthy", "p")]);
    tokio::time::timeout(Duration::from_secs(1), healthy.flush())
        .await
        .unwrap()
        .unwrap();
    release.close();
    assert!(flush.await.is_err());
    blocked.flush().await.unwrap();
}

#[tokio::test]
async fn dataset_flush_retains_pending_rows_when_registration_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"org_info":[{"id":"org","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"project":{"id":"p","name":"p"},"dataset":{"id":"d","name":"d"}}),
        ))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_key("key")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .unwrap();
    let dataset = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("p")
        .dataset_name("d")
        .build()
        .unwrap();
    accept(&server).await;
    let row_id = dataset
        .insert(
            braintrust_sdk_rust::DatasetInsert::builder()
                .input(json!("pending row"))
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(dataset.flush().await.is_err());
    let clone = dataset.clone();
    assert_eq!(clone.dataset_id().await.unwrap(), "d");
    dataset.flush().await.unwrap();
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|request| {
            request.url.path() == "/logs3"
                && String::from_utf8_lossy(&request.body).contains(&row_id)
        }));
}

struct Panicking;
#[async_trait::async_trait]
impl TokenProvider for Panicking {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        panic!("test provider panic")
    }
}
#[tokio::test]
async fn provider_panic_fails_its_partition_without_stopping_the_service() {
    let server = MockServer::start().await;
    accept(&server).await;
    let bad = client(&server);
    let good = client(&server);
    bad.log_rows_with_token_provider(Arc::new(Panicking), "org", None, [row("bad", "p")]);
    assert!(bad.flush().await.is_err());
    assert_eq!(bad.dropped_rows().preparation_failed, 1);
    good.log_rows_with_credentials("token", "org", None, [row("good", "p")]);
    good.flush().await.unwrap();
}

#[tokio::test]
async fn invalid_public_login_state_does_not_stop_other_clients() {
    let server = MockServer::start().await;
    accept(&server).await;
    let bad = client(&server);
    bad.login_state().set(
        "token".into(),
        "org".into(),
        "org".into(),
        "invalid URL".into(),
        server.uri(),
    );
    bad.log_rows_with_credentials("token", "org", None, [row("bad", "p")]);
    assert!(bad.flush().await.is_err());
    assert_eq!(bad.dropped_rows().upload_failed, 1);
    let good = client(&server);
    good.log_rows_with_credentials("token", "org", None, [row("good", "p")]);
    good.flush().await.unwrap();
}

#[tokio::test]
async fn a_project_submitted_after_a_sibling_429_delivers_before_the_retry() {
    let server = MockServer::start().await;
    let first_attempt = Arc::new(tokio::sync::Notify::new());
    let signal = first_attempt.clone();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("throttled-project"))
        .respond_with(move |_: &wiremock::Request| {
            signal.notify_one();
            ResponseTemplate::new(429).insert_header("retry-after", "10")
        })
        .with_priority(1)
        .mount(&server)
        .await;
    accept(&server).await;
    let throttled = client(&server);
    let healthy = client(&server);
    throttled.log_rows_with_credentials(
        "token",
        "same-org",
        None,
        [row("throttled", "throttled-project")],
    );
    tokio::time::timeout(Duration::from_secs(5), first_attempt.notified())
        .await
        .expect("first upload attempt");
    healthy.log_rows_with_credentials(
        "token",
        "same-org",
        None,
        [row("healthy", "healthy-project")],
    );
    tokio::time::timeout(Duration::from_secs(3), healthy.flush())
        .await
        .expect("healthy project must deliver before the throttled project's retry")
        .expect("healthy project delivery");
    let _ = throttled.shutdown(Duration::ZERO).await;
}
