//! The SDK selects its process-wide runtime once. Each scenario starts a fresh process.
use braintrust_sdk_rust::{
    configure_service, BraintrustClient, ServiceConfig, ServiceRuntime, TokenProvider,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn run_child(name: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn work_runs_on_the_runtime_current_at_first_use() {
    run_child("current_runtime_child");
}
#[test]
fn work_runs_on_the_sdk_thread_outside_a_runtime() {
    run_child("no_runtime_child");
}
#[test]
fn work_shares_a_current_thread_runtime() {
    run_child("current_thread_runtime_child");
}
#[test]
fn an_initial_sync_caller_keeps_the_same_sdk_runtime() {
    run_child("initial_sync_caller_child");
}
#[test]
fn work_runs_on_a_configured_runtime() {
    run_child("configured_runtime_child");
}
#[test]
fn later_callers_share_the_first_callers_runtime() {
    run_child("later_callers_child");
}
#[derive(Default)]
struct Recorder {
    threads: Mutex<Vec<String>>,
}
#[async_trait::async_trait]
impl TokenProvider for Recorder {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let thread = std::thread::current().name().unwrap_or_default().to_owned();
        self.threads.lock().unwrap().push(thread);
        Ok("token".into())
    }
}

struct Fixture {
    server_runtime: Runtime,
    server: MockServer,
}
impl Fixture {
    /// The mock server gets its own runtime, so stopping the SDK's runtime
    /// leaves it running.
    fn new() -> Self {
        let server_runtime = Runtime::new().unwrap();
        let server = server_runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/logs3"))
                .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
                .mount(&server)
                .await;
            server
        });
        Self {
            server_runtime,
            server,
        }
    }
    fn client(&self) -> BraintrustClient {
        BraintrustClient::builder()
            .api_url(self.server.uri())
            .app_url(self.server.uri())
            .build_without_login()
            .unwrap()
    }

    fn log(&self, client: &BraintrustClient, provider: &Arc<Recorder>, ids: &[&str]) {
        client.log_rows_with_token_provider(
            provider.clone(),
            "org",
            None,
            ids.iter().map(|id| {
                json!({"id": id, "project_id": "p"})
                    .as_object()
                    .unwrap()
                    .clone()
            }),
        );
    }

    fn uploaded_ids(&self) -> Vec<String> {
        let requests = self
            .server_runtime
            .block_on(self.server.received_requests())
            .unwrap();
        let mut ids: Vec<String> = requests
            .iter()
            .filter(|request| request.url.path() == "/logs3")
            .flat_map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                body["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        ids.sort();
        ids
    }
}
fn named_runtime(name: &str) -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name(name)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn current_runtime_child() {
    // Configuring outside Tokio must not initialize a dedicated runtime.
    configure_service(ServiceConfig::default()).unwrap();
    let fixture = Fixture::new();
    let app = named_runtime("app-runtime");
    let client = app.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());
    app.block_on(async { fixture.log(&client, &provider, &["a"]) });
    // Waiting from outside any runtime, while `app` keeps running.
    assert_eq!(client.flush().blocking_wait().unwrap().delivered(), 1);
    assert_eq!(*provider.threads.lock().unwrap(), ["app-runtime"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn no_runtime_child() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let provider = Arc::new(Recorder::default());
    fixture.log(&client, &provider, &["a"]);
    assert_eq!(client.flush().blocking_wait().unwrap().delivered(), 1);
    assert_eq!(*provider.threads.lock().unwrap(), ["braintrust-delivery"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn configured_runtime_child() {
    let fixture = Fixture::new();
    let configured = named_runtime("configured-runtime");
    configure_service(
        ServiceConfig::builder()
            .runtime(ServiceRuntime::Handle(configured.handle().clone()))
            .build(),
    )
    .unwrap();
    // Being current elsewhere does not override the configured runtime.
    let app = named_runtime("app-runtime");
    let client = app.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());
    fixture.log(&client, &provider, &["a"]);
    assert_eq!(client.flush().blocking_wait().unwrap().delivered(), 1);
    assert_eq!(*provider.threads.lock().unwrap(), ["configured-runtime"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn current_thread_runtime_child() {
    let fixture = Fixture::new();
    // Like `#[tokio::main(flavor = "current_thread")]` or `#[tokio::test]`.
    let app = Builder::new_current_thread().enable_all().build().unwrap();
    let provider = Arc::new(Recorder::default());
    let report = app.block_on(async {
        let client = fixture.client();
        fixture.log(&client, &provider, &["a"]);
        client.flush().await
    });
    assert_eq!(report.unwrap().delivered(), 1);
    let this_thread = std::thread::current().name().unwrap().to_owned();
    assert_eq!(*provider.threads.lock().unwrap(), [this_thread]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn initial_sync_caller_child() {
    let fixture = Fixture::new();
    // Rows submitted outside any runtime upload on the SDK's thread.
    let client = fixture.client();
    let provider = Arc::new(Recorder::default());
    fixture.log(&client, &provider, &["a"]);
    assert_eq!(client.flush().blocking_wait().unwrap().delivered(), 1);
    // A later application runtime still sends work to the SDK thread.
    let app = named_runtime("app-runtime");
    app.block_on(async {
        fixture.log(&client, &provider, &["b"]);
        assert_eq!(client.flush().await.unwrap().delivered(), 1);
    });
    assert_eq!(
        *provider.threads.lock().unwrap(),
        ["braintrust-delivery", "braintrust-delivery"]
    );
    assert_eq!(fixture.uploaded_ids(), ["a", "b"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn later_callers_child() {
    let fixture = Fixture::new();
    let first = named_runtime("first-runtime");
    let a = first.block_on(async { fixture.client() });
    let later = Builder::new_current_thread().enable_all().build().unwrap();
    let b = later.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());

    first.block_on(async { fixture.log(&a, &provider, &["a"]) });
    later.block_on(async {
        b.log_rows_with_token_provider(
            provider.clone(),
            "other-org",
            None,
            [json!({"id": "b", "project_id": "p"})
                .as_object()
                .unwrap()
                .clone()],
        );
    });
    // Drive neither caller task while waiting. The first runtime has its
    // own worker and work from the second caller must also run there.
    a.flush()
        .blocking_wait_timeout(Duration::from_secs(5))
        .unwrap();
    b.flush()
        .blocking_wait_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(fixture.uploaded_ids(), ["a", "b"]);
    assert_eq!(
        *provider.threads.lock().unwrap(),
        ["first-runtime", "first-runtime"]
    );
}
