use braintrust_api_client::{
    backend::{BackendClient, extensions::*},
    types,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn client(server: &MockServer) -> BackendClient {
    BackendClient::new(
        &server.uri(),
        &server.uri(),
        "test-key",
        "00000000-0000-4000-8000-000000000010",
        "test-org",
        "test-client/1",
    )
    .unwrap()
}

#[tokio::test]
async fn typed_function_pagination_preserves_cursor_snapshot_and_query_encoding() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/function"))
        .and(query_param("project_name", "test project & name"))
        .and(query_param("cursor", "cursor/with space"))
        .and(query_param("snapshot", "test-snapshot"))
        .and(header("authorization", "Bearer test-key"))
        .and(|r: &Request| !r.headers.contains_key("x-bt-org-name"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"objects":[],"next_cursor":" next ","snapshot":" snapshot "}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = client(&server)
        .function_page(&FunctionListQuery {
            project_name: Some("test project & name".into()),
            cursor: Some("cursor/with space".into()),
            snapshot: Some("test-snapshot".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(result.objects.is_empty());
    assert_eq!(result.next_cursor.as_deref(), Some("next"));
    assert_eq!(result.snapshot.as_deref(), Some("snapshot"));
}

#[tokio::test]
async fn async_queries_scope_org_headers_and_encode_result_paths() {
    let server = MockServer::start().await;
    for (verb, route, response) in [
        (
            "POST",
            "/btql/async",
            json!({"id":"test-query","status":"queued"}),
        ),
        ("GET", "/btql/async/test-query", json!({"status":"running"})),
        (
            "GET",
            "/btql/async/automation%2Fid/result",
            json!({"status":"succeeded","rows":[],"cursor":null}),
        ),
    ] {
        let mut mock = Mock::given(method(verb))
            .and(path(route))
            .and(header("x-bt-org-name", "test-org"))
            .and(header("authorization", "Bearer test-key"));
        if route.ends_with("/result") {
            mock = mock
                .and(query_param("limit", "100"))
                .and(query_param("cursor", "cursor/with space"));
        }
        mock.respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = client(&server);
    client
        .submit_query(&AsyncQuerySubmission::new("select 1", Some(250)))
        .await
        .unwrap();
    client.query_status("test-query").await.unwrap();
    client
        .query_page("automation/id", 100, Some("cursor/with space"))
        .await
        .unwrap();
}

#[tokio::test]
async fn snapshot_creation_exposes_existing_snapshot_and_uses_generated_models() {
    let server = MockServer::start().await;
    let id = "00000000-0000-4000-8000-000000000001";
    Mock::given(method("POST")).and(path("/v1/dataset_snapshot"))
  .and(body_json(json!({"dataset_id":id,"name":"test-snapshot","xact_id":"123"})))
  .respond_with(ResponseTemplate::new(200).insert_header("x-bt-found-existing","1").set_body_json(json!({"id":id,"dataset_id":id,"name":"test-snapshot","description":null,"created":null,"xact_id":"123"})))
  .expect(1).mount(&server).await;
    let result = client(&server)
        .create_dataset_snapshot(&types::CreateDatasetSnapshot::new(
            id.parse().unwrap(),
            "test-snapshot".into(),
            "123".into(),
        ))
        .await
        .unwrap();
    assert!(result.found_existing);
    assert_eq!(result.dataset_snapshot.xact_id, "123");
}

#[tokio::test]
async fn restore_requires_a_result_transaction_when_rows_change() {
    for (body, succeeds) in [
        (
            json!({"rows_deleted":0,"rows_restored":0,"xact_id":null}),
            true,
        ),
        (
            json!({"rows_deleted":0,"rows_restored":1,"xact_id":null}),
            false,
        ),
        (
            json!({"rows_deleted":1,"rows_restored":1,"xact_id":" 123 "}),
            true,
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/dataset/test-id/restore"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let result = client(&server).restore_dataset("test-id", "111").await;
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(result) = result
            && result.rows_restored > 0
        {
            assert_eq!(result.xact_id.as_deref(), Some("123"));
        }
    }
}

#[tokio::test]
async fn generated_resource_operation_returns_generated_types() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/project")).and(query_param("project_name","test project"))
  .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects":[{"id":"00000000-0000-4000-8000-000000000001","name":"test project","org_id":"00000000-0000-4000-8000-000000000010"}]})))
  .expect(1).mount(&server).await;
    let client = client(&server);
    let response: types::GetProjectResponse = client
        .run(|| {
            client
                .generated()
                .get_project_builder()
                .project_name("test project")
                .send()
        })
        .await
        .unwrap();
    assert_eq!(response.objects[0].name, "test project");
}

#[tokio::test]
async fn generated_automation_patch_preserves_additional_topic_settings() {
    let server = MockServer::start().await;
    let config = json!({"event_type":"topic","sampling_rate":1.0,"facet_functions":[],"topic_map_functions":[],"scope":{"type":"span"},"model":"test-future-model","test_future_setting":{"enabled":true}});
    let body = json!({"config":config});
    Mock::given(method("PATCH")).and(path("/v1/project_automation/test-id"))
        .and(body_json(body.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"00000000-0000-4000-8000-000000000001","project_id":"00000000-0000-4000-8000-000000000002","name":"test automation","config":config})))
        .expect(1).mount(&server).await;
    let request: types::PatchProjectAutomation = serde_json::from_value(body).unwrap();
    let client = client(&server);
    let result = client
        .run(|| {
            client
                .generated()
                .patch_project_automation_id("test-id", Some(request.clone()))
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result.config).unwrap()["test_future_setting"],
        json!({"enabled":true})
    );
}

#[tokio::test]
async fn generated_operations_retry_transient_errors_through_the_api_policy() {
    use braintrust_api_client::backend::{OperationOptions, RetryReason};
    use std::sync::{Arc, Mutex};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/project"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "0")
                .set_body_string("test transient failure"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/project"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects":[]})))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let client = client(&server).with_options(OperationOptions {
        retry: Some(true),
        on_retry: Some(Arc::new(move |reason| {
            observed.lock().unwrap().push(reason)
        })),
        ..Default::default()
    });
    client
        .run(|| client.generated().get_project_builder().send())
        .await
        .unwrap();
    assert_eq!(*reasons.lock().unwrap(), [RetryReason::Status(503)]);
}

#[tokio::test]
async fn generated_operation_timeouts_are_classified_by_the_api_client() {
    use braintrust_api_client::backend::{
        FailureKind, OperationOptions, OperationTimeout, failure_kind,
    };
    use std::time::Duration;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/project"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(json!({"objects":[]})),
        )
        .mount(&server)
        .await;
    let client = client(&server).with_options(OperationOptions {
        timeout: Some(Duration::from_millis(20)),
        ..Default::default()
    });
    let error = client
        .run(|| client.generated().get_project_builder().send())
        .await
        .unwrap_err();
    assert!(error.is::<OperationTimeout>());
    assert_eq!(failure_kind(&error), Some(FailureKind::Unavailable));
}

#[test]
fn resource_definitions_reject_fields_that_would_be_silently_lost() {
    use braintrust_api_client::backend::resource_definition;
    let result = resource_definition::<_, types::CreateProject>(
        json!({"name":"test-project","test_unrepresented_setting":true}),
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("/test_unrepresented_setting")
    );
}

#[tokio::test]
async fn interrupted_success_body_remains_a_retryable_network_error() {
    use braintrust_api_client::backend::{OperationOptions, RetryReason};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let attempt = observed.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).await;
            let response: &[u8] = if attempt == 0 {
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"objects\":[]}"
            };
            stream.write_all(response).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let captured = reasons.clone();
    let client = BackendClient::new(&url, &url, "test-key", "org", "test-org", "test")
        .unwrap()
        .with_options(OperationOptions {
            retry: Some(true),
            on_retry: Some(Arc::new(move |reason| {
                captured.lock().unwrap().push(reason)
            })),
            ..Default::default()
        });
    let result = client
        .run(|| client.generated().get_project_builder().send())
        .await
        .unwrap();
    assert!(result.objects.is_empty());
    assert_eq!(*reasons.lock().unwrap(), [RetryReason::Network]);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    server.abort();
}
