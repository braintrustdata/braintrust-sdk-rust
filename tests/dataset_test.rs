//! Integration tests for Datasets API.

use braintrust_sdk_rust::{BraintrustClient, DatasetInsert};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Helper to create a mock login response
fn mock_login_response(orgs: &[(&str, &str)]) -> ResponseTemplate {
    let org_info: Vec<_> = orgs
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({ "org_info": org_info }))
}

#[tokio::test]
async fn dataset_builder_requires_project_name() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let result = client
        .dataset_builder()
        .await
        .unwrap()
        .dataset_name("test-dataset")
        .build();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(
        err.to_string(),
        "project_name is required but was not provided"
    );
}

#[tokio::test]
async fn dataset_builder_requires_dataset_name() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let result = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("test-project")
        .build();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(
        err.to_string(),
        "dataset_name is required but was not provided"
    );
}

#[tokio::test]
async fn dataset_insert_flushes_to_logs_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-123", "name": "test-project" },
            "dataset": { "id": "ds-456", "name": "test-dataset" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-123" }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let dataset = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("test-project")
        .dataset_name("test-dataset")
        .build()
        .expect("dataset builder");

    // Insert a record
    let record_id = dataset
        .insert(
            DatasetInsert::builder()
                .input(json!({"question": "What is 2+2?"}))
                .expected(json!({"answer": "4"}))
                .build()
                .expect("record"),
        )
        .await;

    assert!(!record_id.is_empty());

    // Flush to send to API
    dataset.flush().await.expect("flush");
    client.flush().await.expect("client flush");

    // Give the background worker time to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn dataset_fetch_returns_records() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-123", "name": "test-project" },
            "dataset": { "id": "ds-456", "name": "test-dataset" }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": "rec-1",
                    "input": {"question": "What is 2+2?"},
                    "expected": {"answer": "4"}
                },
                {
                    "id": "rec-2",
                    "input": {"question": "What is 3+3?"},
                    "expected": {"answer": "6"}
                }
            ],
            "cursor": null
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let dataset = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("test-project")
        .dataset_name("test-dataset")
        .build()
        .expect("dataset builder");

    // Fetch records
    let iter = dataset.fetch(Some(10)).await.expect("fetch");
    let records = iter.collect().await.expect("collect records");

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id(), "rec-1");
    assert_eq!(records[1].id(), "rec-2");
}

#[tokio::test]
async fn dataset_summarize_returns_summary() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-123", "name": "test-project" },
            "dataset": { "id": "ds-456", "name": "test-dataset" }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/dataset-summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project_name": "test-project",
            "dataset_name": "test-dataset",
            "project_id": "proj-123",
            "dataset_id": "ds-456",
            "project_url": "https://braintrust.dev/proj-123",
            "dataset_url": "https://braintrust.dev/ds-456",
            "data_summary": {
                "total_records": 42
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let dataset = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("test-project")
        .dataset_name("test-dataset")
        .build()
        .expect("dataset builder");

    let summary = dataset.summarize().await.expect("summarize");

    assert_eq!(summary.project_name(), "test-project");
    assert_eq!(summary.dataset_name(), "test-dataset");
    assert_eq!(summary.project_id(), Some("proj-123"));
    assert_eq!(summary.dataset_id(), Some("ds-456"));
    assert_eq!(summary.total_records(), Some(42));
}

async fn isolated_client(server: &MockServer) -> BraintrustClient {
    BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap()
}
fn registration() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project","name":"project"},"dataset":{"id":"dataset","name":"dataset"}}))
}
fn dataset(client: &BraintrustClient) -> braintrust_sdk_rust::Dataset<BraintrustClient> {
    client
        .dataset_builder_with_credentials("token", "tenant")
        .project_name("project")
        .dataset_name("dataset")
        .build()
        .unwrap()
}

#[tokio::test]
async fn failed_dataset_registration_is_retryable_and_clone_registration_is_shared() {
    let server = MockServer::start().await;
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
        .respond_with(registration())
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    let clone = dataset.clone();
    assert!(matches!(
        dataset.dataset_id().await,
        Err(braintrust_sdk_rust::BraintrustError::Api { status: 503, .. })
    ));
    let (first, second) = tokio::join!(dataset.dataset_id(), clone.dataset_id());
    assert_eq!(first.unwrap(), "dataset");
    assert_eq!(second.unwrap(), "dataset");
    assert_eq!(dataset.clone().dataset_id().await.unwrap(), "dataset");
    assert!(!client.is_logged_in().await);
}

#[tokio::test]
async fn pagination_failure_is_an_error_and_retry_preserves_the_failed_cursor() {
    use wiremock::matchers::body_partial_json;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(body_partial_json(json!({"query":{"cursor":"next"}})))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(body_partial_json(json!({"query":{"cursor":"next"}})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"second"}],"cursor":null})),
        )
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"first"}],"cursor":"next"})),
        )
        .with_priority(3)
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    let mut iterator = dataset.fetch(Some(1)).await.unwrap();
    assert_eq!(iterator.next().await.unwrap().unwrap().id, "first");
    assert!(matches!(
        iterator.next().await,
        Err(braintrust_sdk_rust::BraintrustError::Api { status: 503, .. })
    ));
    assert_eq!(iterator.next().await.unwrap().unwrap().id, "second");
    assert!(iterator.next().await.unwrap().is_none());
    let requests = server.received_requests().await.unwrap();
    let cursors: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/btql")
        .map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["query"]["cursor"]
                .clone()
        })
        .collect();
    assert_eq!(cursors, vec![json!(null), json!("next"), json!("next")]);
}

#[tokio::test]
async fn collecting_records_propagates_page_errors_instead_of_returning_partial_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"first"}],"cursor":"next"})),
        )
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let records = dataset(&client).fetch(None).await.unwrap().collect().await;
    assert!(matches!(
        records,
        Err(braintrust_sdk_rust::BraintrustError::Api { status: 401, .. })
    ));
}

#[tokio::test]
async fn empty_pages_with_a_continuation_do_not_end_iteration() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[],"cursor":"next"})))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"record"}],"cursor":null})),
        )
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let records = dataset(&client)
        .fetch(Some(0))
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "record");
    for request in server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/btql")
    {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["query"]["limit"],
            1
        );
    }
}

#[tokio::test]
async fn a_non_advancing_cursor_is_reported_and_does_not_poison_a_later_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"first"}],"cursor":"stuck"})),
        )
        .with_priority(1)
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"second"}],"cursor":null})),
        )
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    let mut iterator = dataset.fetch(None).await.unwrap();
    assert_eq!(iterator.next().await.unwrap().unwrap().id, "first");
    assert!(
        matches!(iterator.next().await, Err(braintrust_sdk_rust::BraintrustError::Background(message)) if message.contains("did not advance"))
    );
    assert_eq!(iterator.next().await.unwrap().unwrap().id, "second");
    assert!(iterator.next().await.unwrap().is_none());
}
