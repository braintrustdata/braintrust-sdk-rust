//! Which runtime runs SDK work. The service is process-wide, so each scenario
//! runs in its own subprocess with the default runtime selection.
use braintrust_sdk_rust::{
    configure_service, BraintrustClient, ServiceConfig, ServiceRuntime, TokenProvider,
};
use serde_json::json;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use wiremock::{
    matchers::{body_string_contains, method, path},
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
fn uploads_run_on_the_runtime_that_submitted_their_rows() {
    run_child("submitter_runtime_child");
}
#[test]
fn work_runs_on_a_configured_runtime() {
    run_child("configured_runtime_child");
}
#[test]
fn rows_in_flight_when_the_runtime_stops_are_delivered_on_another() {
    run_child("stopped_runtime_child");
}
#[test]
fn rows_with_a_confirmed_upload_are_not_sent_again_after_the_runtime_stops() {
    run_child("interrupted_upload_child");
}
#[test]
fn work_from_one_runtime_survives_another_stopping() {
    run_child("caller_operation_child");
}
#[test]
fn an_idle_runtime_does_not_hold_up_another_runtimes_uploads() {
    run_child("idle_runtime_child");
}
#[test]
fn an_idle_runtimes_connections_are_not_reused_elsewhere() {
    run_child("idle_connection_child");
}
#[test]
fn a_background_login_interrupted_by_its_runtime_stopping_resumes() {
    run_child("interrupted_login_child");
}

/// Records the thread each token request runs on. While `stall` holds a
/// sender, the next request signals it and never finishes.
#[derive(Default)]
struct Recorder {
    threads: Mutex<Vec<String>>,
    stall: Mutex<Option<mpsc::Sender<()>>>,
}
#[async_trait::async_trait]
impl TokenProvider for Recorder {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let thread = std::thread::current().name().unwrap_or_default().to_owned();
        self.threads.lock().unwrap().push(thread);
        let stall = self.stall.lock().unwrap().take();
        if let Some(stall) = stall {
            stall.send(()).unwrap();
            std::future::pending::<()>().await;
        }
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
    fn wait_for(&self, condition: impl Fn(&Self) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
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
    fn logins(&self) -> usize {
        self.server_runtime
            .block_on(self.server.received_requests())
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/api/apikey/login")
            .count()
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
fn stopped_runtime_child() {
    let fixture = Fixture::new();
    let app = named_runtime("app-runtime");
    let client = app.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());
    let (stalled, entered) = mpsc::channel();
    *provider.stall.lock().unwrap() = Some(stalled);
    app.block_on(async { fixture.log(&client, &provider, &["a", "b"]) });
    // The job holding both rows is mid-flight when its runtime stops.
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    app.shutdown_timeout(Duration::from_secs(5));
    let report = client.flush().blocking_wait().unwrap();
    assert_eq!(report.delivered(), 2);
    assert_eq!(fixture.uploaded_ids(), ["a", "b"]);
    assert_eq!(
        *provider.threads.lock().unwrap(),
        ["app-runtime", "braintrust-delivery"]
    );
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn interrupted_login_child() {
    let fixture = Fixture::new();
    fixture.server_runtime.block_on(
        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"org_info": [{"id": "org-id", "name": "Org"}]}))
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&fixture.server),
    );
    let app = named_runtime("app-runtime");
    // Built inside `app`: `build()` starts the SDK when called, so calling it
    // as `block_on`'s argument would start it outside any runtime.
    let client = app
        .block_on(async {
            BraintrustClient::builder()
                .api_key("key")
                .api_url(fixture.server.uri())
                .app_url(fixture.server.uri())
                .build()
                .await
        })
        .unwrap();
    // Stop the runtime while the first login request is waiting for its response.
    fixture.wait_for(|fixture| fixture.logins() == 1);
    app.shutdown_timeout(Duration::from_secs(5));
    let state = client.wait_for_login().blocking_wait().unwrap();
    assert_eq!(state.org_id().as_deref(), Some("org-id"));
    assert_eq!(fixture.logins(), 2);
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
fn submitter_runtime_child() {
    let fixture = Fixture::new();
    // Rows submitted outside any runtime upload on the SDK's thread.
    let client = fixture.client();
    let provider = Arc::new(Recorder::default());
    fixture.log(&client, &provider, &["a"]);
    assert_eq!(client.flush().blocking_wait().unwrap().delivered(), 1);
    // Rows submitted from an application runtime upload there.
    let app = named_runtime("app-runtime");
    app.block_on(async {
        fixture.log(&client, &provider, &["b"]);
        assert_eq!(client.flush().await.unwrap().delivered(), 1);
    });
    assert_eq!(
        *provider.threads.lock().unwrap(),
        ["braintrust-delivery", "app-runtime"]
    );
    assert_eq!(fixture.uploaded_ids(), ["a", "b"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn interrupted_upload_child() {
    let fixture = Fixture::new();
    // Rows a and b form one batch. The server rejects it as too large, so the
    // SDK splits it: a's request succeeds, and b's is still waiting for its
    // response when the runtime stops.
    fixture.server_runtime.block_on(async {
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(body_string_contains(r#""id":"a""#))
            .and(body_string_contains(r#""id":"b""#))
            .respond_with(ResponseTemplate::new(413))
            .with_priority(1)
            .mount(&fixture.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(body_string_contains(r#""id":"b""#))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{}")
                    .set_delay(Duration::from_secs(30)),
            )
            .up_to_n_times(1)
            .with_priority(2)
            .mount(&fixture.server)
            .await;
    });
    let app = named_runtime("app-runtime");
    let client = app.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());
    app.block_on(async { fixture.log(&client, &provider, &["a", "b"]) });
    // The rejected pair, then a, then b.
    fixture.wait_for(|fixture| fixture.uploaded_ids() == ["a", "a", "b", "b"]);
    app.shutdown_timeout(Duration::from_secs(5));
    let report = client.flush().blocking_wait().unwrap();
    assert_eq!(report.delivered(), 1, "only b was still outstanding");
    // Besides the rejected pair: a once, and b again because its outcome was unknown.
    assert_eq!(fixture.uploaded_ids(), ["a", "a", "b", "b", "b"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn caller_operation_child() {
    let fixture = Fixture::new();
    fixture.server_runtime.block_on(
        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"org_info": [{"id": "org-id", "name": "Org"}]}))
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&fixture.server),
    );
    // Like two concurrent tests: `first` has an upload in flight, while
    // `second` waits for a login, when `first` ends.
    let first = named_runtime("first-runtime");
    let client = first.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());
    let (stalled, entered) = mpsc::channel();
    *provider.stall.lock().unwrap() = Some(stalled);
    first.block_on(async { fixture.log(&client, &provider, &["a"]) });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = named_runtime("second-runtime");
    let uri = fixture.server.uri();
    let login = second.spawn(async move {
        BraintrustClient::builder()
            .api_key("key")
            .api_url(&uri)
            .app_url(&uri)
            .blocking_login(true)
            .build()
            .await
    });
    fixture.wait_for(|fixture| fixture.logins() == 1);
    first.shutdown_timeout(Duration::from_secs(5));
    let other = second.block_on(login).unwrap().unwrap();
    assert!(other.is_logged_in());
    assert_eq!(fixture.logins(), 1, "the login was never interrupted");
    // The interrupted upload moved to the SDK's thread.
    client.flush().blocking_wait().unwrap();
    assert_eq!(fixture.uploaded_ids(), ["a"]);
    assert_eq!(
        *provider.threads.lock().unwrap(),
        ["first-runtime", "braintrust-delivery"]
    );
    // Uploads from the runtime still running stay there.
    let provider = Arc::new(Recorder::default());
    second.block_on(async {
        fixture.log(&other, &provider, &["b"]);
        assert_eq!(other.flush().await.unwrap().delivered(), 1);
    });
    assert_eq!(*provider.threads.lock().unwrap(), ["second-runtime"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn idle_runtime_child() {
    let fixture = Fixture::new();
    let healthy = named_runtime("healthy-runtime");
    // Two clients whose rows share an org, credential and connection profile.
    let idle_client = healthy.block_on(async { fixture.client() });
    let client = healthy.block_on(async { fixture.client() });
    let provider = Arc::new(Recorder::default());
    // A current-thread runtime that is kept alive but no longer driven.
    let idle = Builder::new_current_thread().enable_all().build().unwrap();
    idle.block_on(async { fixture.log(&idle_client, &provider, &["idle"]) });
    let report = healthy.block_on(async {
        fixture.log(&client, &provider, &["healthy"]);
        tokio::time::timeout(Duration::from_secs(5), client.flush()).await
    });
    assert_eq!(report.unwrap().unwrap().delivered(), 1);
    assert_eq!(fixture.uploaded_ids(), ["healthy"]);
    // The idle runtime's row uploads once that runtime runs again.
    idle.block_on(async { idle_client.flush().await.unwrap() });
    assert_eq!(fixture.uploaded_ids(), ["healthy", "idle"]);
}

#[test]
#[ignore = "executed in an isolated subprocess"]
fn idle_connection_child() {
    let fixture = Fixture::new();
    let provider = Arc::new(Recorder::default());
    // Leaves a pooled keep-alive connection that only `idle` can drive.
    let idle = Builder::new_current_thread().enable_all().build().unwrap();
    idle.block_on(async {
        let client = fixture.client();
        fixture.log(&client, &provider, &["a"]);
        client.flush().await.unwrap();
    });
    let client = fixture.client();
    fixture.log(&client, &provider, &["b"]);
    let started = std::time::Instant::now();
    let report = client
        .flush()
        .blocking_wait_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(report.delivered(), 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(idle);
}
