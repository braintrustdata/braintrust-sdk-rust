use braintrust_sdk_rust::BraintrustClient;
use serde_json::{json, Map, Value};
use std::time::Duration;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn row(id: &str) -> Map<String, Value> {
    json!({"id":id,"project_id":"project","log_id":"g"})
        .as_object()
        .unwrap()
        .clone()
}

async fn credentialed_row_upload_stops_at_the_first_end_of_input() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .skip_login(true)
        .build()
        .await
        .unwrap();
    let rows = [Some(row("first")), None, Some(row("after-end"))]
        .into_iter()
        .scan((), |_, row| row);

    client.log_rows_with_credentials("test-key", "org-id", None, rows);
    assert_eq!(client.flush().await.unwrap().delivered(), 1);
}

#[tokio::test]
async fn flush_and_shutdown_cover_writes_waiting_for_login() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("test-key")
        .build()
        .await
        .unwrap();

    drop(client.log_rows([row("covered")]));
    let flush = client.flush();
    let shutdown = client.shutdown(Duration::from_secs(5));
    assert!(client.log_rows([row("late")]).await.is_err());
    assert_eq!(flush.await.unwrap().delivered(), 1);
    assert_eq!(shutdown.await.unwrap().delivered(), 1);

    let requests = server.received_requests().await.unwrap();
    let body = requests
        .iter()
        .find(|request| request.url.path() == "/logs3")
        .unwrap();
    let body: Value = serde_json::from_slice(&body.body).unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["rows"][0]["id"], "covered");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("test-key")
        .build()
        .await
        .unwrap();
    let write = client.log_rows([row("cancelled")]);
    let flush = client.flush();
    assert!(client.shutdown(Duration::from_millis(10)).await.is_err());
    assert!(tokio::time::timeout(Duration::from_millis(300), write)
        .await
        .unwrap()
        .is_err());
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(300), flush)
            .await
            .unwrap(),
        Err(braintrust_sdk_rust::FlushError::Execution(
            braintrust_sdk_rust::ExecutionError::TimedOut
        ))
    ));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("test-key")
        .build()
        .await
        .unwrap();
    drop(client.log_rows([row("first-failure")]));
    drop(client.log_rows([row("second-failure")]));
    let error = client.flush().await.unwrap_err();
    assert!(
        matches!(error, braintrust_sdk_rust::FlushError::Delivery(failure) if failure.failed() == 2)
    );

    credentialed_row_upload_stops_at_the_first_end_of_input().await;
    pending_login_writes_respect_client_capacity().await;
    pending_row_calls_publish_in_call_order().await;
}

async fn pending_login_writes_respect_client_capacity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("key")
        .with_queue_max_size(1)
        .build()
        .await
        .unwrap();
    let first = client.log_rows([row("first")]);
    let warnings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(WarningRecorder(warnings.clone()), || {
        assert!(client.log_rows([row("second")]).submission().is_err());
        assert!(client.log_rows([row("third")]).submission().is_err());
    });
    {
        let warnings = warnings.lock().unwrap();
        assert_eq!(
            warnings.len(),
            1,
            "repeated drops should share the throttled warning"
        );
        assert!(warnings[0].contains("Log queue full"));
    }
    assert_eq!(client.dropped_rows().queue_full, 2);
    first.await.unwrap();
    client.flush().await.unwrap();
}

async fn pending_row_calls_publish_in_call_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("key")
        .blocking_login(true)
        .build()
        .await
        .unwrap();
    let first_client = client.clone();
    let (started, entered) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let first = std::thread::spawn(move || {
        first_client.log_rows(std::iter::once_with(move || {
            started.send(()).unwrap();
            resume.recv().unwrap();
            let mut row = row("same");
            row.insert("output".into(), json!("first"));
            row
        }))
    });
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut update = row("same");
    update.insert("output".into(), json!("second"));
    let mut second = client.log_rows([update]);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    first.join().unwrap().await.unwrap();
    second.await.unwrap();
    client.flush().await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let outputs: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            body["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["output"].clone())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(outputs, vec![json!("first"), json!("second")]);
}

struct WarningRecorder(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl tracing::Subscriber for WarningRecorder {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push(message.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
