use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, EnqueueError, ExecutionError, ParentSpanInfo, ShutdownError,
    WriteError,
};
use std::{sync::Arc, time::Duration};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn held_response(
    arrived: Arc<tokio::sync::Notify>,
    status: u16,
) -> impl Fn(&wiremock::Request) -> ResponseTemplate {
    move |_| {
        arrived.notify_one();
        ResponseTemplate::new(status).set_delay(Duration::from_secs(60))
    }
}

#[tokio::test]
async fn one_deadline_fails_active_http_and_ingestion_then_rejects_new_work() {
    let server = MockServer::start().await;
    let http_arrived = Arc::new(tokio::sync::Notify::new());
    let write_arrived = Arc::new(tokio::sync::Notify::new());
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(held_response(http_arrived.clone(), 200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(held_response(write_arrived.clone(), 204))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let dataset = client
        .dataset_builder_with_credentials("token", "tenant")
        .project_name("p")
        .dataset_name("d")
        .build()
        .unwrap();
    let pending = dataset.clone();
    let http = tokio::spawn(async move { pending.dataset_id().await });
    let span = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build();
    let write = span.end();
    tokio::time::timeout(Duration::from_secs(5), async {
        http_arrived.notified().await;
        write_arrived.notified().await;
    })
    .await
    .unwrap();
    let first = client.shutdown(Duration::from_millis(20));
    let concurrent = client.shutdown(Duration::from_secs(60));
    for shutdown in [first, concurrent] {
        assert!(matches!(
            shutdown.await,
            Err(ShutdownError::Execution(ExecutionError::TimedOut))
        ));
    }
    assert!(matches!(
        write.await,
        Err(WriteError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        http.await.unwrap(),
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        dataset.dataset_id().await,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    let fresh = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build();
    assert!(matches!(
        fresh.end().await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn background_login_is_accepted_work_covered_by_shutdown() {
    let server = MockServer::start().await;
    let arrived = Arc::new(tokio::sync::Notify::new());
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(held_response(arrived.clone(), 200))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_key("token")
        .blocking_login(false)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(!client.is_logged_in().await);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
