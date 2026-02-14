use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use braintrust_sdk_rust::Logs3BatchUploader;
use serde_json::{json, Map, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn make_row(id: &str, payload_chars: usize) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("id".to_string(), Value::String(id.to_string()));
    row.insert("span_id".to_string(), Value::String(format!("span-{id}")));
    row.insert(
        "root_span_id".to_string(),
        Value::String(format!("root-{id}")),
    );
    row.insert(
        "project_id".to_string(),
        Value::String("proj-id".to_string()),
    );
    row.insert("log_id".to_string(), Value::String("g".to_string()));
    row.insert(
        "input".to_string(),
        Value::Object(
            [(
                "text".to_string(),
                Value::String("x".repeat(payload_chars.max(1))),
            )]
            .into_iter()
            .collect(),
        ),
    );
    row
}

struct RejectMultiRowResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for RejectMultiRowResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_else(|_| json!({}));
        let row_count = body
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| rows.len())
            .unwrap_or(0);
        if row_count > 1 {
            ResponseTemplate::new(413).set_body_string(r#"{"message":"Request Too Long"}"#)
        } else {
            ResponseTemplate::new(200).set_body_string("{}")
        }
    }
}

#[tokio::test]
async fn uploader_splits_batches_on_413_without_overflow() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(RejectMultiRowResponder {
            calls: Arc::clone(&calls),
        })
        .mount(&server)
        .await;

    let mut uploader = Logs3BatchUploader::new(server.uri(), "test-key", None).expect("uploader");
    let rows = vec![make_row("1", 32), make_row("2", 32), make_row("3", 32)];

    let result = uploader.upload_rows(&rows, 100).await.expect("upload");
    assert_eq!(result.rows_uploaded, 3);
    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "expected multiple /logs3 attempts due to 413 splitting"
    );

    let requests = server.received_requests().await.unwrap();
    let overflow_calls = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3/overflow")
        .count();
    assert_eq!(overflow_calls, 0);
}

#[tokio::test]
async fn uploader_uses_overflow_protocol_for_large_payloads() {
    let server = MockServer::start().await;
    let signed_upload_url = format!("{}/signed-upload", server.uri());

    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"logs3_payload_max_bytes": 512})),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3/overflow"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "method": "PUT",
            "signedUrl": signed_upload_url,
            "headers": {},
            "key": "overflow-key-1"
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/signed-upload"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let mut uploader = Logs3BatchUploader::new(server.uri(), "test-key", None).expect("uploader");
    let rows = vec![make_row("big", 8_000)];

    let result = uploader.upload_rows(&rows, 100).await.expect("upload");
    assert_eq!(result.rows_uploaded, 1);

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request.url.path() == "/logs3/overflow"),
        "expected logs3/overflow call"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.url.path() == "/signed-upload"),
        "expected signed upload PUT call"
    );

    let logs_request = requests
        .iter()
        .find(|request| request.url.path() == "/logs3")
        .expect("logs3 request");
    let body: Value = serde_json::from_slice(&logs_request.body).expect("logs3 json");
    assert_eq!(body["rows"]["type"], "logs3_overflow");
    assert_eq!(body["rows"]["key"], "overflow-key-1");
}
