//! Eager dataset operations own admission through all of their subrequests.
use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, Dataset, DatasetInsert, EnqueueError,
};
use serde_json::json;
use std::{future::IntoFuture, sync::Arc, time::Duration};
use wiremock::{
    matchers::{body_partial_json, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn dataset(client: &BraintrustClient) -> Dataset {
    client
        .dataset_builder_with_credentials("token", "tenant")
        .project_name("project")
        .dataset_name("dataset")
        .build()
        .unwrap()
}
fn record() -> DatasetInsert {
    DatasetInsert::builder().input(json!(1)).build().unwrap()
}
async fn registration(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project":{"id":"project","name":"project"},
            "dataset":{"id":"dataset","name":"dataset"}
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn synchronous_callers_issue_unobserved_operations_and_shutdown_drains_them() {
    let server = MockServer::start().await;
    registration(&server).await;
    let arrived = Arc::new(tokio::sync::Notify::new());
    let observed = arrived.clone();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(move |_: &wiremock::Request| {
            observed.notify_one();
            ResponseTemplate::new(204)
        })
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dataset-summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project_name":"project", "dataset_name":"dataset",
            "project_id":"project", "dataset_id":"dataset"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();
    let shutdown = std::thread::spawn(move || {
        let client = futures::executor::block_on(
            BraintrustClient::builder()
                .skip_login(true)
                .app_url(&uri)
                .api_url(&uri)
                .build()
                .into_future(),
        )
        .unwrap();
        let dataset = dataset(&client);
        for operation in [dataset.update("old", record()), dataset.delete("old")] {
            assert!(operation.is_ok());
            drop(operation);
        }
        let insert = dataset.insert(record());
        assert!(insert.is_ok());
        drop(insert);
        drop(dataset.summarize());
        let shutdown = client.shutdown(Duration::from_secs(5));
        drop(dataset);
        drop(client);
        shutdown
    })
    .join()
    .unwrap();
    // Network progress occurs before observing any accepted operation token.
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    assert_eq!(shutdown.await.unwrap().delivered, 0);
}

#[tokio::test]
async fn saturated_operations_drain_after_shutdown_without_nested_http_admission() {
    let server = MockServer::start().await;
    registration(&server).await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let dataset = dataset(&client);
    // No selected-runtime polling between these issuance calls and shutdown.
    let ids: Vec<_> = (0..1023).map(|_| dataset.dataset_id()).collect();
    assert!(ids.iter().all(|id| id.is_ok()));
    let insert = dataset.insert(record());
    assert!(insert.is_ok());
    let full = dataset.fetch(None);
    assert!(matches!(
        &*full,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    let shutdown = client.shutdown(Duration::from_secs(5));
    let closed = dataset.dataset_id();
    assert!(matches!(
        &*closed,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    shutdown.await.unwrap();
    for id in ids {
        assert_eq!(id.await.unwrap(), "dataset");
    }
    assert!(!insert.await.unwrap().is_empty());
    assert!(matches!(
        full.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        closed.await,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    // One shared registration followed by the accepted write, even though
    // public admission was closed before either request started.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn unobserved_cursor_commands_consume_records_in_order_and_collect_across_shutdown() {
    let server = MockServer::start().await;
    registration(&server).await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data":[{"id":"first"}], "cursor":"next"
        })))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(body_partial_json(json!({"query":{"cursor":"next"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data":[{"id":"second"},{"id":"third"}]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let dataset = dataset(&client);
    let mut cursor = dataset.fetch(Some(1)).await.unwrap();
    drop(cursor.next());
    let next = cursor.next();
    let rest = cursor.collect();
    assert!(next.is_ok());
    assert!(rest.is_ok());
    let shutdown = client.shutdown(Duration::from_secs(5));
    drop(dataset);
    drop(client);
    shutdown.await.unwrap();
    assert_eq!(next.await.unwrap().unwrap().id, "second");
    assert_eq!(
        rest.await
            .unwrap()
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["third"]
    );
}

#[tokio::test]
async fn rejected_cursor_admission_preserves_its_position_and_allows_retry() {
    let server = MockServer::start().await;
    registration(&server).await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data":[{"id":"first"},{"id":"second"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let dataset = dataset(&client);
    let mut cursor = dataset.fetch(None).await.unwrap();
    let first = cursor.next();
    let ids: Vec<_> = (0..1023).map(|_| dataset.dataset_id()).collect();
    let rejected = cursor.next();
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    for id in ids {
        id.await.unwrap();
    }
    assert_eq!(first.await.unwrap().unwrap().id, "first");
    assert!(matches!(
        rejected.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    let retry = cursor.next();
    assert!(retry.is_ok());
    assert_eq!(retry.await.unwrap().unwrap().id, "second");
    assert!(cursor.next().await.unwrap().is_none());
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}
