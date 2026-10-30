use braintrust_sdk_rust::{BraintrustClient, SpanLog};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn dataset_queries_and_summaries_use_the_discovered_api() {
    let app = MockServer::start().await;
    let fallback = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"org_info":[
            {"id":"org", "name":"my-org", "api_url":api.uri()}
        ]})))
        .expect(1)
        .mount(&app)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .and(header("authorization", "Bearer key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project":{"id":"project", "name":"project"},
            "dataset":{"id":"dataset", "name":"dataset"}
        })))
        .expect(1)
        .mount(&app)
        .await;
    for (cursor, next, id) in [
        (None, Some("page2"), "first"),
        (Some("page2"), None, "second"),
    ] {
        let mut query = json!({"from":"dataset('dataset')", "limit":1});
        if let Some(cursor) = cursor {
            query["cursor"] = json!(cursor);
        }
        Mock::given(method("POST")).and(path("/btql"))
            .and(header("authorization", "Bearer key"))
            .and(body_json(json!({"query":query, "fmt":"json"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data":[{"id":id, "input":{"question":"hello"}, "_xact_id":"version"}], "cursor":next
            }))).expect(1).mount(&api).await;
    }
    Mock::given(method("GET")).and(path("/dataset-summary"))
        .and(query_param("dataset_id", "dataset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project_name":"project", "dataset_name":"dataset", "project_id":"project", "dataset_id":"dataset", "data_summary":{"total_records":2}
        }))).expect(1).mount(&api).await;
    let client = BraintrustClient::builder()
        .api_key("key")
        .app_url(app.uri())
        .api_url(fallback.uri())
        .org_name("my-org")
        .blocking_login(true)
        .build()
        .await
        .unwrap();
    let dataset = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("project")
        .dataset_name("dataset")
        .build()
        .unwrap();
    let records = dataset.fetch(Some(1)).await.unwrap().collect().await;
    assert_eq!(
        records
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(records[0].xact_id.as_deref(), Some("version"));
    assert_eq!(dataset.summarize().await.unwrap().total_records(), Some(2));
    assert!(fallback.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn experiment_summary_uses_api_host_and_accepts_a_missing_base() {
    let app = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/experiment/register"))
        .and(header("authorization", "Bearer per-request-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project":{"id":"project", "name":"project"}, "experiment":{"id":"experiment", "name":"experiment"}
        }))).expect(1).mount(&app).await;
    Mock::given(method("POST"))
        .and(path("/api/base_experiment/get_id"))
        .respond_with(ResponseTemplate::new(400).set_body_string("no base experiment"))
        .expect(1)
        .mount(&app)
        .await;
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .and(query_param("experiment_id", "experiment"))
        .and(header("authorization", "Bearer per-request-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"scores":{"accuracy":{"score":0.9}}, "metrics":{}})),
        )
        .expect(1)
        .mount(&api)
        .await;
    let client = BraintrustClient::builder()
        .api_key("unused-key")
        .skip_login(true)
        .app_url(app.uri())
        .api_url(api.uri())
        .build()
        .await
        .unwrap();
    let experiment = client
        .experiment_builder_with_credentials("per-request-key", "org")
        .project_name("project")
        .build()
        .unwrap();
    let summary = experiment.summarize().await.unwrap();
    assert_eq!(summary.scores()["accuracy"].score(), 0.9);
}

#[tokio::test]
async fn queued_overflow_retries_the_reference_without_reuploading_or_leaking_credentials() {
    let app = MockServer::start().await;
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"org_info":[{"id":"org", "name":"my-org"}]})),
        )
        .expect(1)
        .mount(&app)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project"}})))
        .expect(1)
        .mount(&app)
        .await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .and(header("x-bt-org-name", "my-org"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"logs3_payload_max_bytes":512})),
        )
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3/overflow"))
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
        .and(header("authorization", "Bearer upload-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "method":"PUT", "signedUrl":format!("{}/signed", storage.uri()), "key":"overflow-key"
        })))
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("PUT"))
        .and(path("/signed"))
        .and(|request: &Request| {
            !request.headers.contains_key("authorization")
                && !request.headers.contains_key("x-bt-org-name")
        })
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&storage)
        .await;
    let reference =
        json!({"rows":{"type":"logs3_overflow", "key":"overflow-key"}, "api_version":2});
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("x-bt-org-name", "my-org"))
        .and(body_json(reference.clone()))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("x-bt-org-name", "my-org"))
        .and(body_json(reference))
        .and(header("authorization", "Bearer upload-key"))
        .respond_with(ResponseTemplate::new(200))
        .with_priority(2)
        .expect(1)
        .mount(&api)
        .await;
    let client = BraintrustClient::builder()
        .api_key("upload-key")
        .org_name("my-org")
        .blocking_login(true)
        .app_url(app.uri())
        .api_url(api.uri())
        .checked_delivery(true)
        .build()
        .await
        .unwrap();
    let span = client
        .span_builder()
        .await
        .unwrap()
        .project_name("project")
        .build();
    span.log(
        SpanLog::builder()
            .input(json!("x".repeat(8000)))
            .build()
            .unwrap(),
    );
    span.flush().await.unwrap();
}
