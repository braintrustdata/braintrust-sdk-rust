use braintrust_api_client::{
    backend::{BackendClient, HttpError, extensions::*},
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
        .function_page(&{
            let mut options = FunctionListQuery::default();
            options.project_name = Some("test project & name".into());
            options.cursor = Some("cursor/with space".into());
            options.snapshot = Some("test-snapshot".into());
            options
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
    let client = client(&server).with_options({
        let mut options = OperationOptions::default();
        options.retry = Some(true);
        options.on_retry = Some(Arc::new(move |reason| {
            observed.lock().unwrap().push(reason)
        }));
        options
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
    let client = client(&server).with_options({
        let mut options = OperationOptions::default();
        options.timeout = Some(Duration::from_millis(20));
        options
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
    let error =
        resource_definition::<_, types::PatchProject>(json!({"test_unrepresented_setting": null}))
            .unwrap_err();
    assert!(error.to_string().contains("/test_unrepresented_setting"));
    let patch: types::PatchProject = resource_definition(json!({"description": null})).unwrap();
    assert_eq!(
        serde_json::to_value(patch).unwrap(),
        json!({"description": null})
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
        .with_options({
            let mut options = OperationOptions::default();
            options.retry = Some(true);
            options.on_retry = Some(Arc::new(move |reason| {
                captured.lock().unwrap().push(reason)
            }));
            options
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

#[tokio::test]
async fn attachment_lookup_decodes_metadata_and_upload_status() {
    let server = MockServer::start().await;
    let client = client(&server);
    let download_url = "https://storage.example/download?signature=download";
    let embed_url = "https://storage.example/embed?signature=embed";

    for (key, status, content_length, expected_status) in [
        (
            "complete/key",
            json!({"upload_status": "done"}),
            Some(512),
            AttachmentUploadStatus::Done,
        ),
        (
            "in-progress/key",
            json!({"upload_status": "uploading"}),
            None,
            AttachmentUploadStatus::Uploading,
        ),
        (
            "failed/key",
            json!({"upload_status": "error", "error_message": "upload failed"}),
            None,
            AttachmentUploadStatus::Error {
                error_message: Some("upload failed".into()),
            },
        ),
        (
            "failed-without-message/key",
            json!({"upload_status": "error"}),
            None,
            AttachmentUploadStatus::Error {
                error_message: None,
            },
        ),
    ] {
        let mut response = json!({
            "status": status,
            "downloadUrl": download_url,
            "embedUrl": embed_url,
        });
        if let Some(length) = content_length {
            response["contentLength"] = json!(length);
        }
        Mock::given(method("GET"))
            .and(path("/attachment"))
            .and(header("authorization", "Bearer test-key"))
            .and(query_param("key", key))
            .and(query_param("filename", "report + notes.pdf"))
            .and(query_param("content_type", "application/pdf"))
            .and(query_param("org_id", "org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;

        let metadata = client
            .get_attachment(&AttachmentRequest::new(
                key.into(),
                "report + notes.pdf".into(),
                "application/pdf".into(),
                "org".into(),
            ))
            .await
            .unwrap();
        assert_eq!(metadata.status, expected_status);
        assert_eq!(metadata.content_length, content_length);
        assert_eq!(metadata.download_url, download_url);
        assert_eq!(metadata.embed_url, embed_url);
    }
}

#[tokio::test]
async fn missing_attachment_preserves_http_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/attachment"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .get_attachment(&AttachmentRequest::new(
            "missing".into(),
            "file.bin".into(),
            "application/octet-stream".into(),
            "org".into(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<HttpError>().unwrap().status, 404);
}

#[tokio::test]
async fn attachment_upload_keeps_api_credentials_out_of_object_storage() {
    let server = MockServer::start().await;
    let storage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/attachment"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"signedUrl":format!("{}/object",storage.uri()),"headers":{"x-upload":"signed"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/object"))
        .and(header("x-upload", "signed"))
        .and(|request: &Request| {
            !request.headers.contains_key("authorization") && request.body == b"attachment-bytes"
        })
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&storage)
        .await;
    Mock::given(method("POST"))
        .and(path("/attachment/status"))
        .and(body_json(
            json!({"key":"key","org_id":"org","status":{"upload_status":"done"}}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .upload_attachment(
            &AttachmentRequest::new(
                "key".into(),
                "file.bin".into(),
                "application/octet-stream".into(),
                "org".into(),
            ),
            b"attachment-bytes".to_vec(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_attachment_upload_does_not_report_completion_and_can_retry() {
    let server = MockServer::start().await;
    let storage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/attachment"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"signedUrl":format!("{}/object",storage.uri()),"headers":{}}),
            ),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/attachment/status"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/object"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&storage)
        .await;
    let request = AttachmentRequest::new(
        "key".into(),
        "file.bin".into(),
        "application/octet-stream".into(),
        "org".into(),
    );
    let backend = client(&server);
    assert!(
        backend
            .upload_attachment(&request, b"data".to_vec())
            .await
            .is_err()
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.url.path() == "/attachment/status")
    );
    storage.reset().await;
    Mock::given(method("PUT"))
        .and(path("/object"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&storage)
        .await;
    backend
        .upload_attachment(&request, b"data".to_vec())
        .await
        .unwrap();
}

#[tokio::test]
async fn attachment_uploads_reuse_the_storage_connection_across_client_clones() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let server = MockServer::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let storage = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        for _ in 0..2 {
            let mut size = 0;
            loop {
                let mut line = String::new();
                assert!(
                    stream.read_line(&mut line).await.unwrap() > 0,
                    "storage connection was not reused"
                );
                if line == "\r\n" {
                    break;
                }
                let line = line.to_ascii_lowercase();
                assert!(!line.starts_with("authorization:"));
                if let Some(value) = line.strip_prefix("content-length:") {
                    size = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut data = vec![0; size];
            stream.read_exact(&mut data).await.unwrap();
            assert_eq!(data, b"data");
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        }
    });
    Mock::given(method("POST"))
        .and(path("/attachment"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"signedUrl":format!("http://{address}/bytes"),"headers":{}})),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/attachment/status"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    let backend = client(&server);
    let clone = backend.clone();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for (index, backend) in [&backend, &clone].into_iter().enumerate() {
            backend
                .upload_attachment(
                    &AttachmentRequest::new(
                        format!("key-{index}"),
                        "file.bin".into(),
                        "application/octet-stream".into(),
                        "org".into(),
                    ),
                    b"data".to_vec(),
                )
                .await
                .unwrap();
        }
        storage.await.unwrap();
    })
    .await
    .expect("uploads should reuse one storage connection");
}
