//! Each configuration scenario gets a new process and therefore a new singleton.
use braintrust_sdk_rust::{configure_service, BraintrustClient, ServiceConfig, TokenProvider};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[test]
fn global_limits_and_configuration_are_process_wide() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "configuration_child", "--ignored", "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "executed in an isolated subprocess"]
async fn configuration_child() {
    let config = ServiceConfig::builder()
        .preparation_timeout(Duration::from_secs(1))
        .max_concurrent_requests(2)
        .max_rows(6)
        .max_org_rows(6)
        .max_client_rows(2)
        .build();
    configure_service(config.clone()).unwrap();
    let server = MockServer::start().await;
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let observed = arrivals.clone();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(move |_: &wiremock::Request| {
            observed.lock().unwrap().push(Instant::now());
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(Duration::from_millis(100))
        })
        .mount(&server)
        .await;
    let clients: Vec<_> = (0..6)
        .map(|_| {
            BraintrustClient::builder()
                .api_url(server.uri())
                .app_url(server.uri())
                .checked_delivery(true)
                .build_without_login()
                .unwrap()
        })
        .collect();
    configure_service(config).unwrap();
    assert!(configure_service(ServiceConfig::default()).is_err());
    for (i, client) in clients.iter().enumerate() {
        client.log_rows_with_credentials(
            "token",
            "org",
            None,
            [json!({"id":i,"project_id":format!("p-{i}")})
                .as_object()
                .unwrap()
                .clone()],
        );
    }
    // Global budget is full even though this client has local space.
    clients[0].log_rows_with_credentials(
        "token",
        "org",
        None,
        [json!({"id":"dropped","project_id":"p-0"})
            .as_object()
            .unwrap()
            .clone()],
    );
    assert_eq!(clients[0].dropped_rows().queue_full, 1);
    for result in futures::future::join_all(
        clients
            .iter()
            .map(|client| async move { client.flush().await }),
    )
    .await
    {
        result.unwrap();
    }
    let times = arrivals.lock().unwrap().clone();
    assert_eq!(times.len(), 6);
    for i in 2..times.len() {
        assert!(
            times[i].duration_since(times[i - 2]) >= Duration::from_millis(90),
            "concurrency exceeded configured process cap"
        );
    }
    // Delivered rows release global and local admission capacity.
    clients[0].log_rows_with_credentials(
        "token",
        "org",
        None,
        [json!({"id":"new","project_id":"p-0"})
            .as_object()
            .unwrap()
            .clone()],
    );
    clients[0].flush().await.unwrap();
    assert_eq!(clients[0].dropped_rows().queue_full, 1);
    // A credential execution deadline drops the stalled provider future.
    struct Cancelled(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    struct Stalled(Mutex<Option<tokio::sync::oneshot::Sender<()>>>);
    #[async_trait::async_trait]
    impl TokenProvider for Stalled {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            let _cancelled = Cancelled(self.0.lock().unwrap().take());
            std::future::pending().await
        }
    }
    let (cancelled, observed) = tokio::sync::oneshot::channel();
    clients[0].log_rows_with_token_provider(
        Arc::new(Stalled(Mutex::new(Some(cancelled)))),
        "org",
        None,
        [json!({"id":"stalled", "project_id":"stalled"})
            .as_object()
            .unwrap()
            .clone()],
    );
    assert!(clients[0].flush().await.is_err());
    tokio::time::timeout(Duration::from_secs(2), observed)
        .await
        .unwrap()
        .unwrap();
}
