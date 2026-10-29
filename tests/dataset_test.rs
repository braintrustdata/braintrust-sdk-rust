//! Integration tests for Datasets API.

use braintrust_sdk_rust::{BraintrustClient, DatasetFields, DatasetInsert};
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
async fn dataset_insert_is_acknowledged_without_flush() {
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
                .build(),
        )
        .await
        .expect("insert");

    assert!(!record_id.is_empty());

    let requests = server.received_requests().await.unwrap();
    let writes: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&writes[0].body).unwrap(),
        json!({
            "api_version": 2, "rows": [{"id":record_id, "dataset_id":"ds-456",
            "input":{"question":"What is 2+2?"}, "expected":{"answer":"4"}}]
        })
    );
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
fn dataset(client: &BraintrustClient) -> braintrust_sdk_rust::Dataset {
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
    assert_eq!(iterator.next().await.unwrap().unwrap().id(), "first");
    assert!(matches!(
        iterator.next().await,
        Err(braintrust_sdk_rust::BraintrustError::Api { status: 503, .. })
    ));
    assert_eq!(iterator.next().await.unwrap().unwrap().id(), "second");
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
    assert_eq!(records[0].id(), "record");
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
    assert_eq!(iterator.next().await.unwrap().unwrap().id(), "first");
    assert!(
        matches!(iterator.next().await, Err(braintrust_sdk_rust::BraintrustError::Background(message)) if message.contains("did not advance"))
    );
    assert_eq!(iterator.next().await.unwrap().unwrap().id(), "second");
    assert!(iterator.next().await.unwrap().is_none());
}

fn record(value: i32) -> DatasetInsert {
    DatasetInsert::builder().input(json!(value)).build()
}

#[tokio::test]
async fn mutations_have_dataset_wire_shapes_and_share_registration_across_clones() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(3)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    let id = dataset
        .insert(
            DatasetInsert::builder()
                .input(json!(1))
                .expected(json!(2))
                .metadata(serde_json::Map::from_iter([(
                    "source".into(),
                    json!("test"),
                )]))
                .tags(vec!["sample".into()])
                .build(),
        )
        .await
        .unwrap();
    dataset.clone().update(&id, record(3).into()).await.unwrap();
    dataset.delete(&id).await.unwrap();
    // Neither barrier registers or replays dataset work.
    dataset.flush().await.unwrap();
    client.flush().await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .filter(|r| r.url.path() == "/logs3")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(
        bodies,
        vec![
            json!({"api_version":2,"rows":[{"dataset_id":"dataset","id":id,"input":1,"expected":2,"metadata":{"source":"test"},"tags":["sample"]}]}),
            json!({"api_version":2,"rows":[{"dataset_id":"dataset","id":id,"input":3,"_is_merge":true}]}),
            json!({"api_version":2,"rows":[{"dataset_id":"dataset","id":id,"_object_delete":true}]}),
        ]
    );
    for request in requests {
        assert_eq!(request_header(&request, "authorization"), "Bearer token");
        assert_eq!(request_header(&request, "x-bt-org-name"), "tenant");
    }
}

#[tokio::test]
async fn partial_updates_send_only_supplied_fields_and_preserve_null_and_empty_values() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .expect(1)
        .mount(&server)
        .await;
    let patches = [
        (
            DatasetFields::builder().expected(4).build(),
            json!({"expected":4}),
        ),
        (
            DatasetFields::builder()
                .metadata(serde_json::Map::from_iter([(
                    "source".into(),
                    json!("reviewed"),
                )]))
                .build(),
            json!({"metadata":{"source":"reviewed"}}),
        ),
        (
            DatasetFields::builder()
                .tags(vec!["reviewed".into()])
                .build(),
            json!({"tags":["reviewed"]}),
        ),
        (
            DatasetFields::builder().expected(json!(null)).build(),
            json!({"expected":null}),
        ),
        (
            DatasetFields::builder().input(json!(null)).build(),
            json!({"input":null}),
        ),
        (
            DatasetFields::builder().tags(vec![]).build(),
            json!({"tags":[]}),
        ),
        (
            DatasetFields::builder()
                .metadata(serde_json::Map::new())
                .build(),
            json!({"metadata":{}}),
        ),
    ];
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(patches.len() as u64)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    for (fields, expected_fields) in patches {
        dataset.update("existing", fields).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        let mut row = expected_fields;
        row["dataset_id"] = json!("dataset");
        row["id"] = json!("existing");
        row["_is_merge"] = json!(true);
        assert_eq!(body, json!({"api_version":2,"rows":[row]}));
    }
}

#[tokio::test]
async fn inserts_preserve_explicit_null_input() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    let id = dataset
        .insert(DatasetInsert::builder().input(json!(null)).build())
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    assert_eq!(
        body,
        json!({"api_version":2,"rows":[{"dataset_id":"dataset","id":id,"input":null}]})
    );
}

#[tokio::test]
async fn mutation_errors_are_direct_and_do_not_replay_or_poison_later_operations() {
    for status in [401, 413, 429, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/dataset/register"))
            .respond_with(registration())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(status).set_body_string("rejected"))
            .with_priority(1)
            .up_to_n_times(3)
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(204))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        let client = isolated_client(&server).await;
        let dataset = dataset(&client);
        let insert = dataset.insert(record(1)).await.map(|_| ());
        let update = dataset.update("old", record(2).into()).await;
        let delete = dataset.delete("old").await;
        for result in [insert, update, delete] {
            match result {
                Err(braintrust_sdk_rust::BraintrustError::Api {
                    status: actual,
                    message,
                    ..
                }) => {
                    assert_eq!(actual, status);
                    assert_eq!(message, "rejected");
                }
                other => panic!("expected API error, got {other:?}"),
            }
        }
        dataset.flush().await.unwrap();
        let id = dataset.insert(record(4)).await.unwrap();
        dataset.flush().await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 5); // One registration and four one-attempt writes.
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[4].body).unwrap()["rows"][0]
                ["id"],
            id
        );
    }
}

#[tokio::test]
async fn registration_failure_prevents_mutation_and_flush_does_not_register_a_dataset() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(401))
        .expect(3)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    dataset.flush().await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(dataset.insert(record(1)).await.is_err());
    assert!(dataset.update("old", record(2).into()).await.is_err());
    assert!(dataset.delete("old").await.is_err());
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() == "/api/dataset/register"));
}

#[tokio::test]
async fn resource_operations_preserve_configured_planes_prefixes_and_transport_headers() {
    use wiremock::matchers::{header, query_param};
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/control/api/dataset/register"))
        .and(header("authorization", "Bearer token"))
        .and(header("x-bt-org-name", "tenant"))
        .respond_with(registration())
        .expect(1)
        .mount(&control)
        .await;
    Mock::given(method("POST"))
        .and(path("/data/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&data)
        .await;
    Mock::given(method("POST"))
        .and(path("/data/btql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"record"}]})))
        .expect(1)
        .mount(&data)
        .await;
    Mock::given(method("GET")).and(path("/data/dataset-summary"))
        .and(query_param("dataset_id", "dataset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project_name":"project","dataset_name":"dataset","project_id":"project","dataset_id":"dataset"
        }))).expect(1).mount(&data).await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(format!("{}/control", control.uri()))
        .api_url(format!("{}/data", data.uri()))
        .user_agent("resource-client/1")
        .build()
        .await
        .unwrap();
    let dataset = dataset(&client);
    dataset.insert(record(1)).await.unwrap();
    assert_eq!(
        dataset.fetch(None).await.unwrap().collect().await.unwrap()[0].id(),
        "record"
    );
    dataset.summarize().await.unwrap();
    let requests = data.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert_eq!(request_header(&request, "authorization"), "Bearer token");
        assert_eq!(request_header(&request, "x-bt-org-name"), "tenant");
        assert_eq!(request_header(&request, "user-agent"), "resource-client/1");
    }
    assert_eq!(control.received_requests().await.unwrap().len(), 1);
}

fn request_header<'a>(request: &'a wiremock::Request, name: &str) -> &'a str {
    request.headers.get(name).unwrap().to_str().unwrap()
}

#[tokio::test]
async fn dataset_flush_observes_the_shared_trace_queue_and_retires_its_failure() {
    use braintrust_sdk_rust::{FlushError, ParentSpanInfo, SpanLog};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let dataset = dataset(&client);
    let span = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::Experiment {
            object_id: "experiment".into(),
        })
        .build();
    let write = span.log(SpanLog::builder().input(json!(1)).build().unwrap());
    let flush = dataset.flush();
    assert!(matches!(flush.await, Err(FlushError::Delivery(failure)) if failure.failed() == 1));
    assert!(write.await.is_err());
    assert_eq!(dataset.flush().await.unwrap().delivered(), 0);
    let requests = server.received_requests().await.unwrap();
    assert!(requests
        .iter()
        .all(|r| r.url.path() != "/api/dataset/register"));
}
