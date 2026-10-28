use std::sync::Arc;

use braintrust_sdk_rust::{
    BraintrustClient, DeliveryError, EnqueueError, FlushError, ParentSpanInfo, SpanHandle, SpanLog,
    WriteError,
};
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn client(server: &MockServer) -> BraintrustClient {
    BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .unwrap()
}
fn span(client: &BraintrustClient) -> SpanHandle {
    client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build()
}
async fn delivery_error(
    completion: braintrust_sdk_rust::Completion<braintrust_sdk_rust::DeliveryReceipt, WriteError>,
) -> Arc<DeliveryError> {
    assert!(completion.is_ok(), "observation was issued successfully");
    match completion.await {
        Err(WriteError::Delivery(error)) => error,
        other => panic!("expected delivery error, got {other:?}"),
    }
}

#[tokio::test]
async fn repeated_ends_share_a_retired_failure_without_replaying_or_poisoning_fresh_work() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server).await;
    let finished = span(&client);
    let first = finished.end_with_time(123.0);
    let second = finished.clone().end_with_time(456.0);
    let flush = client.flush();
    let error = delivery_error(first).await;
    assert!(Arc::ptr_eq(&error, &delivery_error(second).await));
    match flush.await {
        Err(FlushError::Delivery(failure)) => {
            assert_eq!(failure.failed, 1);
            assert!(Arc::ptr_eq(&error, &failure.cause));
        }
        other => panic!("expected failed flush, got {other:?}"),
    }
    assert_eq!(client.flush().await.unwrap().delivered, 0);
    assert!(Arc::ptr_eq(&error, &delivery_error(finished.end()).await));
    assert_eq!(finished.end_time(), Some(123.0));
    let fresh = span(&client);
    fresh.end().await.unwrap();
    assert_eq!(client.flush().await.unwrap().delivered, 0);
    let weak = Arc::downgrade(&error);
    drop(error);
    drop(finished);
    assert!(
        weak.upgrade().is_none(),
        "only the live finished span may retain its outcome"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .count(),
        2
    );
}

#[tokio::test]
async fn finishing_a_parent_does_not_finish_its_child_and_explicit_updates_remain_independent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(4)
        .mount(&server)
        .await;
    let client = client(&server).await;
    let parent = span(&client);
    let context = parent.export().unwrap();
    let exported = context.to_str();
    let child = client
        .span_builder_with_credentials("token", "org")
        .parent_info(context.to_parent_span_info().unwrap())
        .build();
    parent.end_with_time(10.0).await.unwrap();
    assert_eq!(child.end_time(), None);
    child
        .log(
            SpanLog::builder()
                .input(json!("child still active"))
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    child.end_with_time(20.0).await.unwrap();
    client
        .update_span_with_credentials(
            "token",
            "org",
            &exported,
            SpanLog::builder()
                .output(json!("explicit patch"))
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    client.flush().await.unwrap();
    let bodies: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/logs3")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(bodies[1]["rows"][0]["input"], "child still active");
    assert!(bodies[1]["rows"][0]["metrics"].get("end").is_none());
    assert_eq!(bodies[2]["rows"][0]["metrics"]["end"], 20.0);
    assert_eq!(bodies[3]["rows"][0]["id"], parent.row_id());
    assert_eq!(bodies[3]["rows"][0]["output"], "explicit patch");
    assert_eq!(parent.end_time(), Some(10.0));
}

#[tokio::test]
async fn final_admission_rejection_is_visible_and_keeps_the_chosen_timestamp() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let span = span(&client);
    client
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    let end = span.end_with_time(123.0);
    assert!(matches!(
        &*end,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        end.await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        span.end_with_time(999.0).await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    assert_eq!(span.end_time(), Some(123.0));
    span.log(SpanLog::builder().input(json!("ignored")).build().unwrap())
        .await
        .unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn named_experiment_span_keeps_its_initial_name_in_the_final_snapshot() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project","name":"p"},"experiment":{"id":"experiment","name":"e"}})))
        .expect(1).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("p")
        .build()
        .unwrap();
    let span = experiment
        .start_span()
        .await
        .unwrap()
        .name("original")
        .build();
    span.end_with_time(123.0).await.unwrap();
    span.log(SpanLog::builder().name("too late").build().unwrap())
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let request = requests.iter().find(|r| r.url.path() == "/logs3").unwrap();
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["rows"][0]["span_attributes"]["name"], "original");
}
