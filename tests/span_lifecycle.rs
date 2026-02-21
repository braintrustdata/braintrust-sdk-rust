use braintrust_sdk_rust::{
    extract_anthropic_usage, extract_openai_usage, BraintrustClient, SpanLog,
};
use serde_json::{json, Value};
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
async fn span_lifecycle_flushes_to_logs_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

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
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let span = client
        .span_builder()
        .await
        .unwrap()
        .project_name("demo-project")
        .build();
    span.log(
        SpanLog::builder()
            .name("integration-span")
            .input(Value::String("input".into()))
            .output(Value::String("output".into()))
            .build()
            .expect("build"),
    )
    .await;
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
