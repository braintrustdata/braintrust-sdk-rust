use braintrust_sdk_rust::{wrap_stream_with_span, BraintrustClient};
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn client(server: &MockServer) -> BraintrustClient {
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"test-project"}})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
    BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .unwrap()
}
async fn rows(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/logs3")
        .flat_map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect()
}
// Error values need neither Display nor Debug; original provider items survive.
struct ProviderError;

#[tokio::test]
async fn exhaustion_finalizes_once_after_role_only_and_usage_only_chunks() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let span = client
        .span_builder_with_credentials("token", "org")
        .project_name("test-project")
        .build();
    let chunks = vec![
        json!({"choices":[{"delta":{"role":"assistant"}}]}),
        json!({"usage":{"prompt_tokens":3}}),
        json!({"choices":[{"delta":{"content":"hello"}}]}),
        json!({"usage":{"prompt_tokens":3,"completion_tokens":2}}),
    ];
    let mut wrapped = wrap_stream_with_span(
        stream::iter(chunks.clone().into_iter().map(Ok::<_, ProviderError>)),
        span.clone(),
    );
    for expected in chunks {
        assert_eq!(wrapped.next().await.map(|r| r.ok()), Some(Some(expected)));
    }
    assert!(wrapped.next().await.is_none());
    let end = span.end_time().unwrap();
    assert!(wrapped.next().await.is_none());
    drop(wrapped);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    assert_eq!(span.end_time(), Some(end));
    let rows = rows(&server).await;
    assert!(rows
        .iter()
        .any(|r| r["output"][0]["message"]["content"] == "hello"));
    assert!(rows
        .iter()
        .any(|r| r["metrics"]["completion_tokens"].as_f64() == Some(2.0)));
    assert!(rows
        .iter()
        .any(|r| r["metrics"].get("time_to_first_token").is_some()));
}

#[tokio::test]
async fn early_drop_submits_partial_output_and_error_without_waiting_for_eof() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let span = client
        .span_builder_with_credentials("token", "org")
        .project_name("test-project")
        .build();
    let inner = stream::iter(vec![
        Ok(json!({"choices":[{"delta":{"content":"partial"}}]})),
        Err(ProviderError),
    ])
    .chain(stream::pending());
    let mut wrapped = wrap_stream_with_span(inner, span.clone());
    assert!(matches!(wrapped.next().await, Some(Ok(_))));
    assert!(matches!(wrapped.next().await, Some(Err(ProviderError))));
    drop(wrapped);
    assert!(span.end_time().is_some());
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    let rows = rows(&server).await;
    assert!(rows
        .iter()
        .any(|r| r["output"][0]["message"]["content"] == "partial"));
    assert!(rows.iter().any(|r| r["error"] == "stream failed"));
    assert!(rows.iter().any(|r| r["metrics"].get("end").is_some()));
}

#[tokio::test]
async fn role_only_stream_does_not_record_time_to_first_token() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let span = client
        .span_builder_with_credentials("token", "org")
        .project_name("test-project")
        .build();
    let wrapped = wrap_stream_with_span(
        stream::iter([Ok::<_, ProviderError>(
            json!({"choices":[{"delta":{"role":"assistant"}}]}),
        )]),
        span,
    );
    let _ = wrapped.collect::<Vec<_>>().await;
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    assert!(rows(&server)
        .await
        .iter()
        .all(|r| r["metrics"].get("time_to_first_token").is_none()));
}
