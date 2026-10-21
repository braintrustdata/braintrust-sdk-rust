use braintrust_sdk_rust::{eval::*, BraintrustClient, ExecutionError};
use std::{future::IntoFuture, io::Read, net::TcpListener, sync::Arc, time::Duration};

fn command(mode: &str) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "initialization_child", "--nocapture"])
        .env("BT_INIT_CHILD", mode)
        .env("BT_EVAL_LOCAL", "1")
        .env("BT_EVAL_NO_SEND_LOGS", "0")
        .env("BT_EVAL_LIST", "0")
        .env("BT_EVAL_JSONL", "1")
        .env("BT_EVAL_TERMINATE_ON_FAILURE", "0")
        .env_remove("BT_EVAL_FILTER_PARSED")
        .env_remove("BT_EVAL_SSE_SOCK")
        .env_remove("BT_EVAL_SSE_ADDR")
        .env_remove("BRAINTRUST_API_KEY")
        .env_remove("BRAINTRUST_ORG_NAME");
    command
}
fn check(command: &mut std::process::Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn options() -> EvalOpts<usize, usize> {
    EvalOpts::builder()
        .name("init".into())
        .source(
            vec![Case {
                input: 1,
                ..Default::default()
            }]
            .into(),
        )
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &usize| Ok(*input))),
                vec![],
            )
            .unwrap(),
        )
        .quiet(false)
        .build()
}
#[test]
fn initialization_child() {
    let Ok(mode) = std::env::var("BT_INIT_CHILD") else {
        return;
    };
    match mode.as_str() {
        "filters-json" | "filters-pattern" => {
            let token = BtEvalRunner::from_env();
            assert!(token.is_err());
            let Err(error) = futures::executor::block_on(token.into_future()) else {
                panic!("invalid filters must reject")
            };
            if mode == "filters-json" {
                assert!(matches!(error, BtInitError::Filters(_)));
            } else {
                assert!(matches!(error, BtInitError::FilterPattern { index: 0, .. }));
            }
            assert!(std::error::Error::source(&error).is_some());
        }
        "list" => {
            let token = BtEvalRunner::from_env();
            assert!(token.is_ok());
            let mut runner = futures::executor::block_on(token.into_future()).unwrap();
            assert!(runner.is_list_mode());
            drop(runner.eval("project", options()));
            assert!(futures::executor::block_on(runner.finish().into_future()).unwrap());
        }
        "sse-error" | "unix-error" => {
            let token = BtEvalRunner::from_env();
            assert!(token.is_ok());
            let Err(error) = futures::executor::block_on(token.into_future()) else {
                panic!("failed SSE must not fall back")
            };
            match error {
                BtInitError::Sse { transport, source } => {
                    assert_eq!(
                        transport,
                        if mode == "unix-error" {
                            "Unix socket"
                        } else {
                            "TCP"
                        }
                    );
                    assert!(matches!(
                        source.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ));
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
        "snapshot" | "lost" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let client = runtime
                .block_on(
                    BraintrustClient::builder()
                        .runtime(Some(runtime.handle().clone()))
                        .skip_login(true)
                        .build()
                        .into_future(),
                )
                .unwrap();
            let token = BtEvalRunner::with_client(Arc::new(client));
            assert!(token.is_ok());
            if mode == "lost" {
                drop(runtime);
                let Err(error) = futures::executor::block_on(token.into_future()) else {
                    panic!("runtime loss must fail")
                };
                assert!(matches!(
                    error,
                    BtInitError::Execution(ExecutionError::WorkerLost)
                ));
            } else {
                // No initialization task can run until this runtime is driven.
                std::env::set_var("BT_EVAL_LIST", "1");
                std::env::set_var("BT_EVAL_FILTER_PARSED", "invalid");
                std::env::set_var("BT_EVAL_SSE_ADDR", "127.0.0.1:9");
                std::env::set_var("BT_EVAL_LOCAL", "0");
                runtime.block_on(async {
                    let mut runner = token.await.unwrap();
                    assert!(!runner.is_list_mode());
                    assert!(runner.no_send_logs());
                    runner.eval("project", options()).await.unwrap();
                    assert!(runner.finish().await.unwrap());
                });
            }
        }
        "record" => {
            let token = BtEvalRunner::from_env();
            assert!(token.is_ok());
            futures::executor::block_on(async {
                let mut runner = token.await.unwrap();
                assert!(!runner.no_send_logs());
                runner.eval("project", options()).await.unwrap();
                assert!(runner.finish().await.unwrap());
            });
        }
        "drop" => {
            let client = futures::executor::block_on(
                BraintrustClient::builder()
                    .skip_login(true)
                    .build()
                    .into_future(),
            )
            .unwrap();
            let token = BtEvalRunner::with_client(Arc::new(client.clone()));
            assert!(token.is_ok());
            drop(token);
            futures::executor::block_on(client.shutdown(Duration::from_secs(5)).into_future())
                .unwrap();
        }
        other => panic!("unknown mode {other}"),
    }
}
#[test]
fn invalid_filters_fail_before_client_or_socket_setup() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for (mode, filters) in [
        ("filters-json", "{"),
        ("filters-json", r#"[{"path":[1],"pattern":"a"}]"#),
        ("filters-pattern", r#"[{"path":["name"],"pattern":"("}]"#),
    ] {
        check(
            command(mode)
                .env("BT_EVAL_LOCAL", "0")
                .env("BT_EVAL_FILTER_PARSED", filters)
                .env(
                    "BT_EVAL_SSE_ADDR",
                    listener.local_addr().unwrap().to_string(),
                ),
        );
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
#[test]
fn list_initialization_ignores_sse_and_needs_no_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    check(
        command("list")
            .env("BT_EVAL_LIST", "1")
            .env("BT_EVAL_LOCAL", "0")
            .env(
                "BT_EVAL_SSE_ADDR",
                listener.local_addr().unwrap().to_string(),
            ),
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
#[test]
fn constructor_configuration_is_captured_before_first_poll() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    check(command("snapshot").env(
        "BT_EVAL_SSE_ADDR",
        listener.local_addr().unwrap().to_string(),
    ));
    let (mut stream, _) = listener
        .accept()
        .expect("captured SSE endpoint must be used");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    assert!(text.contains("event: start\n"));
    assert!(text.contains("event: summary\n"));
    assert!(!text.contains("event: error\n"));
}
#[test]
fn dropped_constructor_observer_still_connects_and_shutdown_joins_cleanup() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    check(command("drop").env(
        "BT_EVAL_SSE_ADDR",
        listener.local_addr().unwrap().to_string(),
    ));
    let (mut stream, _) = listener
        .accept()
        .expect("accepted initialization must survive observer drop");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(
        bytes.is_empty(),
        "discarded runner closes its socket without running evals"
    );
}
#[test]
fn external_runtime_loss_before_first_poll_does_not_connect_elsewhere() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    check(command("lost").env(
        "BT_EVAL_SSE_ADDR",
        listener.local_addr().unwrap().to_string(),
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_failure_precedes_recording_client_login() {
    let server = wiremock::MockServer::start().await;
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    check(
        command("sse-error")
            .env("BT_EVAL_LOCAL", "0")
            .env("BRAINTRUST_API_KEY", "test-key")
            .env("BRAINTRUST_APP_URL", server.uri())
            .env("BRAINTRUST_API_URL", server.uri())
            .env("BT_EVAL_SSE_ADDR", address.to_string()),
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
#[cfg(unix)]
#[test]
fn explicit_unix_failure_does_not_fall_back_to_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let missing =
        std::path::PathBuf::from("/tmp").join(format!("bt-init-{}.sock", uuid::Uuid::new_v4()));
    check(command("unix-error").env("BT_EVAL_SSE_SOCK", missing).env(
        "BT_EVAL_SSE_ADDR",
        listener.local_addr().unwrap().to_string(),
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recording_constructor_composes_login_and_evaluation_with_the_same_client() {
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    for (endpoint, body) in [
        (
            "/api/apikey/login",
            json!({"org_info":[{"id":"org","name":"org"}]}),
        ),
        (
            "/api/experiment/register",
            json!({"project":{"id":"project","name":"project"},"experiment":{"id":"experiment","name":"experiment"}}),
        ),
        ("/logs3", json!({})),
    ] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    check(
        command("record")
            .env("BT_EVAL_LOCAL", "0")
            .env("BRAINTRUST_API_KEY", "test-key")
            .env("BRAINTRUST_APP_URL", server.uri())
            .env("BRAINTRUST_API_URL", server.uri())
            .env(
                "BT_EVAL_SSE_ADDR",
                listener.local_addr().unwrap().to_string(),
            ),
    );
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    assert!(text.contains("event: start\n"));
    assert!(text.contains("\"experiment_id\":\"experiment\""));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/api/apikey/login")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/api/experiment/register")
            .count(),
        1
    );
    assert!(requests.iter().any(|r| r.url.path() == "/logs3"));
}
