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
    let iter = dataset.fetch(Some(10)).await;
    let records = iter.collect().await;

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, "rec-1");
    assert_eq!(records[1].id, "rec-2");
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
