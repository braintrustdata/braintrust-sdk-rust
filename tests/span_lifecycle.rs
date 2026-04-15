use braintrust_sdk_rust::{
    extract_anthropic_usage, extract_openai_usage, BraintrustClient, SpanComponents, SpanLog,
    SpanObjectType,
};
use serde_json::{json, Map, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn span_lifecycle_flushes_to_logs_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-id" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    let span = client
        .span_builder_with_credentials("token", "org-id")
        .org_name("org-name")
        .project_name("demo-project")
        .build();
    span.log(
        SpanLog::builder()
            .name("integration-span")
            .input(Value::String("input".into()))
            .output(Value::String("output".into()))
            .build()
            .expect("build"),
    );
    span.flush().await.expect("flush");
    client.flush().await.expect("client flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 1);

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    assert_eq!(body["api_version"], 2);
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["span_attributes"]["name"], "integration-span");
    assert_eq!(row["project_id"], "proj-id");
}

#[tokio::test]
async fn client_update_span_uses_exported_ids_for_project_logs() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");
    let _ = client.span_builder_with_credentials("token", "org-id");

    let exported = SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: Some("proj-id".to_string()),
        compute_object_metadata_args: None,
        row_id: Some("row-id".to_string()),
        span_id: Some("span-id".to_string()),
        root_span_id: Some("root-id".to_string()),
        span_parents: None,
        propagated_event: None,
    }
    .to_str();

    client
        .update_span(
            &exported,
            SpanLog::builder()
                .output(json!({"status": "updated"}))
                .build()
                .expect("build"),
        )
        .await
        .expect("update");
    client.flush().await.expect("flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 1);

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["id"], "row-id");
    assert_eq!(row["project_id"], "proj-id");
    assert_eq!(row["span_id"], "span-id");
    assert_eq!(row["root_span_id"], "root-id");
    assert_eq!(row["_is_merge"], true);
    assert!(row.get("span_parents").is_none());
}

#[tokio::test]
async fn client_update_span_with_credentials_works_without_priming_login_state() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    let exported = SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: Some("proj-id".to_string()),
        compute_object_metadata_args: None,
        row_id: Some("row-id".to_string()),
        span_id: Some("span-id".to_string()),
        root_span_id: Some("root-id".to_string()),
        span_parents: None,
        propagated_event: None,
    }
    .to_str();

    client
        .update_span_with_credentials(
            "token",
            "org-id",
            &exported,
            SpanLog::builder()
                .output(json!({"status": "updated"}))
                .build()
                .expect("build"),
        )
        .expect("update");
    client.flush().await.expect("flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 1);

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["id"], "row-id");
    assert_eq!(row["project_id"], "proj-id");
    assert_eq!(row["span_id"], "span-id");
    assert_eq!(row["root_span_id"], "root-id");
    assert!(row.get("span_parents").is_none());
}

#[tokio::test]
async fn client_update_span_includes_exported_span_parents() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");
    let _ = client.span_builder_with_credentials("token", "org-id");

    let exported = SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: Some("proj-id".to_string()),
        compute_object_metadata_args: None,
        row_id: Some("row-id".to_string()),
        span_id: Some("span-id".to_string()),
        root_span_id: Some("root-id".to_string()),
        span_parents: Some(vec!["parent-id".to_string()]),
        propagated_event: None,
    }
    .to_str();

    client
        .update_span(
            &exported,
            SpanLog::builder()
                .output(json!({"status": "updated"}))
                .build()
                .expect("build"),
        )
        .await
        .expect("update");
    client.flush().await.expect("flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 1);

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["span_parents"], json!(["parent-id"]));
}

#[tokio::test]
async fn client_update_span_with_credentials_includes_exported_span_parents() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    let exported = SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: Some("proj-id".to_string()),
        compute_object_metadata_args: None,
        row_id: Some("row-id".to_string()),
        span_id: Some("span-id".to_string()),
        root_span_id: Some("root-id".to_string()),
        span_parents: Some(vec!["parent-id".to_string()]),
        propagated_event: None,
    }
    .to_str();

    client
        .update_span_with_credentials(
            "token",
            "org-id",
            &exported,
            SpanLog::builder()
                .output(json!({"status": "updated"}))
                .build()
                .expect("build"),
        )
        .expect("update");
    client.flush().await.expect("flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 1);

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["span_parents"], json!(["parent-id"]));
}

#[tokio::test]
async fn client_update_span_resolves_project_name_from_exported_compute_metadata_args() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-id" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");
    let _ = client.span_builder_with_credentials("token", "org-id");

    let mut compute_object_metadata_args = Map::new();
    compute_object_metadata_args.insert("project_name".to_string(), json!("demo-project"));

    let exported = SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: None,
        compute_object_metadata_args: Some(compute_object_metadata_args),
        row_id: Some("row-id".to_string()),
        span_id: Some("span-id".to_string()),
        root_span_id: Some("root-id".to_string()),
        span_parents: None,
        propagated_event: None,
    }
    .to_str();

    client
        .update_span(
            &exported,
            SpanLog::builder()
                .output(json!({"status": "updated"}))
                .build()
                .expect("build"),
        )
        .await
        .expect("update");
    client.flush().await.expect("flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 1);

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["project_id"], "proj-id");
    assert_eq!(row["span_id"], "span-id");
    assert_eq!(row["root_span_id"], "root-id");
}

#[test]
fn usage_extractors_return_expected_metrics() {
    let openai_usage = extract_openai_usage(&json!({
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "reasoning_tokens": 2
        }
    }));
    assert_eq!(openai_usage.prompt_tokens(), Some(10));
    assert_eq!(openai_usage.completion_tokens(), Some(5));
    assert_eq!(openai_usage.total_tokens(), Some(15));
    assert_eq!(openai_usage.reasoning_tokens(), Some(2));

    let anthropic_usage = extract_anthropic_usage(&json!({
        "usage": {
            "input_tokens": 3,
            "output_tokens": 7
        }
    }));
    assert_eq!(anthropic_usage.prompt_tokens(), Some(3));
    assert_eq!(anthropic_usage.completion_tokens(), Some(7));
    assert_eq!(anthropic_usage.total_tokens(), Some(10));
}
