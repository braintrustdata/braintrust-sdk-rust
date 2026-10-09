use braintrust_sdk_rust::{
    extract_anthropic_usage, extract_openai_usage, BraintrustClient, ParentSpanInfo,
    SpanComponents, SpanLog, SpanObjectType, SpanOrigin,
};
use serde_json::{json, Map, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INTERNAL_OVERRIDE_PAGINATION_KEY_FIELD: &str = "_bt_internal_override_pagination_key";

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
    assert_eq!(row["_is_merge"], true);
    assert!(row.get("span_parents").is_none());
    assert_eq!(row["context"]["span_origin"]["name"], "braintrust.sdk.rust");
    assert_eq!(
        row["context"]["span_origin"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        row["context"]["span_origin"]["instrumentation"],
        json!({"name": "braintrust-rust-sdk"})
    );
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
async fn client_update_span_skip_span_origin_preserves_only_explicit_context() {
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
        .environment("ci", Some("test"))
        .span_origin(SpanOrigin::new().name("client-origin"))
        .build()
        .await
        .expect("client");

    let contexts = [
        (None, None),
        (Some(json!({"custom": {"nested": [1, true]}})), None),
        (
            Some(json!({"span_origin": {"name": "historical-plugin"}, "custom": 42})),
            None,
        ),
        (Some(json!(null)), None),
        (Some(json!("custom context")), None),
        (Some(json!([1, "context"])), None),
        (
            None,
            Some(json!({"span_origin": {"name": "propagated-plugin"}, "source": "parent"})),
        ),
    ];
    for (index, (context, propagated_context)) in contexts.iter().enumerate() {
        let exported = SpanComponents {
            object_type: SpanObjectType::ProjectLogs,
            object_id: Some("proj-id".to_string()),
            compute_object_metadata_args: None,
            row_id: Some(format!("row-{index}")),
            span_id: Some(format!("span-{index}")),
            root_span_id: Some(format!("span-{index}")),
            span_parents: None,
            propagated_event: propagated_context
                .as_ref()
                .map(|context| Map::from_iter([("context".to_string(), context.clone())])),
        }
        .to_str();
        let mut event = SpanLog::builder()
            .skip_span_origin()
            .span_origin(SpanOrigin::new().name("event-origin"))
            .output(json!({"status": "updated"}));
        if let Some(context) = context {
            event = event.context(context.clone());
        }
        client
            .update_span_with_credentials(
                "token",
                "org-id",
                &exported,
                event.build().expect("build"),
            )
            .expect("update");
        client.flush().await.expect("flush");
    }

    let requests = server.received_requests().await.expect("requests");
    let logs_requests: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), contexts.len());
    for (request, (context, propagated_context)) in logs_requests.iter().zip(&contexts) {
        let body: Value = serde_json::from_slice(&request.body).expect("json body");
        let rows = body["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["_is_merge"], true);
        assert_eq!(row["output"], json!({"status": "updated"}));
        assert_eq!(
            row.get("context"),
            context.as_ref().or(propagated_context.as_ref())
        );
    }
}

#[tokio::test]
async fn span_handle_skip_span_origin_persists_through_logs_and_end() {
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
        .environment("ci", Some("test"))
        .span_origin(SpanOrigin::new().name("client-origin"))
        .build()
        .await
        .expect("client");
    let contexts = [
        None,
        Some(json!({"span_origin": {"name": "historical-plugin"}, "custom": 42})),
    ];
    for context in &contexts {
        let span = client
            .span_builder_with_credentials("token", "org-id")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "proj-id".to_string(),
            })
            .span_origin(SpanOrigin::new().name("builder-origin"))
            .build();
        let mut event = SpanLog::builder()
            .span_origin(SpanOrigin::new().name("event-origin"))
            .skip_span_origin()
            .input(json!("input"));
        if let Some(context) = context {
            event = event.context(context.clone());
        }
        span.log(event.build().expect("build"));
        client.flush().await.expect("initial flush");
        span.log(
            SpanLog::builder()
                .output(json!("output"))
                .build()
                .expect("build"),
        );
        client.flush().await.expect("update flush");
        span.end();
        client.flush().await.expect("end flush");
    }

    let requests = server.received_requests().await.expect("requests");
    let logs_requests: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), contexts.len() * 3);
    for (requests, context) in logs_requests.chunks_exact(3).zip(&contexts) {
        for (index, request) in requests.iter().enumerate() {
            let body: Value = serde_json::from_slice(&request.body).expect("json body");
            let rows = body["rows"].as_array().expect("rows");
            assert_eq!(rows.len(), 1);
            let row = &rows[0];
            assert_eq!(row.get("context"), context.as_ref());
            assert_eq!(row["input"], "input");
            assert_eq!(
                row.get("_is_merge").and_then(Value::as_bool),
                (index != 0).then_some(true)
            );
            if index > 0 {
                assert_eq!(row["output"], "output");
            }
            assert_eq!(row["metrics"].get("end").is_some(), index == 2);
        }
    }
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
async fn client_update_span_with_credentials_inherits_exported_propagated_event() {
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
        propagated_event: Some(Map::from_iter([
            (
                INTERNAL_OVERRIDE_PAGINATION_KEY_FIELD.to_string(),
                json!("p07589456150966042624"),
            ),
            (
                "_async_scoring_control".to_string(),
                json!({
                    "kind": "state_override",
                    "state": { "status": "disabled" },
                }),
            ),
            (
                "metadata".to_string(),
                json!({
                    "source": "parent",
                    "nested": { "from_parent": true },
                }),
            ),
            ("metrics".to_string(), json!({ "parent_tokens": 12 })),
            ("tags".to_string(), json!(["propagated"])),
            (
                "span_attributes".to_string(),
                json!({ "purpose": "scorer", "skip_realtime": true }),
            ),
        ])),
    }
    .to_str();

    for output in ["first", "second"] {
        client
            .update_span_with_credentials(
                "token",
                "org-id",
                &exported,
                SpanLog::builder()
                    .name("exported-child")
                    .metadata(Map::from_iter([(
                        "nested".to_string(),
                        json!({ "from_child": true }),
                    )]))
                    .metric("child_latency_ms", 42.0)
                    .tag("request")
                    .output(json!({ "status": output }))
                    .build()
                    .expect("build"),
            )
            .expect("update");
        client.flush().await.expect("flush");
    }

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert_eq!(logs_requests.len(), 2);

    for request in logs_requests {
        let body: Value = serde_json::from_slice(&request.body).expect("json body");
        let row = body["rows"]
            .as_array()
            .and_then(|rows| rows.first())
            .expect("row");
        assert_eq!(
            row[INTERNAL_OVERRIDE_PAGINATION_KEY_FIELD],
            "p07589456150966042624"
        );
        assert_eq!(
            row["_async_scoring_control"],
            json!({
                "kind": "state_override",
                "state": { "status": "disabled" },
            })
        );
        assert_eq!(
            row["metadata"],
            json!({
                "source": "parent",
                "nested": {
                    "from_parent": true,
                    "from_child": true,
                },
            })
        );
        assert_eq!(
            row["metrics"],
            json!({
                "parent_tokens": 12.0,
                "child_latency_ms": 42.0,
            })
        );
        assert_eq!(row["tags"], json!(["request", "propagated"]));
        assert_eq!(
            row["span_attributes"],
            json!({
                "name": "exported-child",
                "purpose": "scorer",
                "skip_realtime": true,
            })
        );
        assert_eq!(row["span_parents"], json!(["parent-id"]));
    }
}

#[tokio::test]
async fn child_span_inherits_parent_propagated_event() {
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

    let span = client
        .span_builder_with_credentials("token", "org-id")
        .parent_info(ParentSpanInfo::FullSpan {
            object_type: SpanObjectType::ProjectLogs,
            object_id: Some("proj-id".to_string()),
            compute_object_metadata_args: None,
            span_id: "parent-span-id".to_string(),
            root_span_id: "root-id".to_string(),
            span_parents: None,
            propagated_event: Some(Map::from_iter([
                (
                    INTERNAL_OVERRIDE_PAGINATION_KEY_FIELD.to_string(),
                    json!("p07589456150966042624"),
                ),
                (
                    "_async_scoring_control".to_string(),
                    json!({
                        "kind": "state_override",
                        "state": { "status": "disabled" },
                    }),
                ),
                ("metadata".to_string(), json!({"source": "parent"})),
                ("tags".to_string(), json!(["propagated"])),
                (
                    "span_attributes".to_string(),
                    json!({ "purpose": "scorer", "skip_realtime": true }),
                ),
            ])),
        })
        .build();
    span.log(
        SpanLog::builder()
            .metadata(Map::from_iter([("child".to_string(), json!(true))]))
            .tag("request")
            .output(json!({"status": "child"}))
            .build()
            .expect("build"),
    );
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
    assert_eq!(
        row[INTERNAL_OVERRIDE_PAGINATION_KEY_FIELD],
        "p07589456150966042624"
    );
    assert_eq!(
        row["_async_scoring_control"],
        json!({
            "kind": "state_override",
            "state": { "status": "disabled" },
        })
    );
    assert_eq!(
        row["metadata"],
        json!({
            "source": "parent",
            "child": true,
        })
    );
    assert_eq!(row["tags"], json!(["request", "propagated"]));
    assert_eq!(row["span_attributes"]["purpose"], "scorer");
    assert_eq!(row["span_attributes"]["skip_realtime"], true);
    assert_eq!(row["span_parents"], json!(["parent-span-id"]));
    assert!(row
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get(INTERNAL_OVERRIDE_PAGINATION_KEY_FIELD))
        .is_none());
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
