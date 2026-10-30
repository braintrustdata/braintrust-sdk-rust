//! Environment builders run in child processes so tests never mutate the
//! environment of a multithreaded test runner.

use std::process::{Command, Output};

use braintrust_sdk_rust::api;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn child_command(test: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", test, "--ignored", "--nocapture"]);
    command
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "child test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn default_builder_uses_environment_key_and_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer environment-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
        .expect(1)
        .mount(&server)
        .await;
    let url = server.uri();
    let output = tokio::task::spawn_blocking(move || {
        child_command("environment_key_child")
            .env("BRAINTRUST_API_KEY", "environment-key")
            .env("BRAINTRUST_API_URL", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_success(output);
}

#[tokio::test]
async fn explicit_configuration_overrides_the_environment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer explicit-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
        .expect(1)
        .mount(&server)
        .await;
    let url = server.uri();
    let output = tokio::task::spawn_blocking(move || {
        child_command("explicit_key_child")
            .env("BRAINTRUST_API_KEY", "unused-environment-key")
            .env("BRAINTRUST_API_URL", "https://unused.example.invalid")
            .env("TEST_API_URL", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_success(output);
}

#[test]
fn default_builder_reports_missing_key() {
    assert_success(
        child_command("missing_key_child")
            .env_remove("BRAINTRUST_API_KEY")
            .env_remove("BRAINTRUST_API_URL")
            .output()
            .unwrap(),
    );
}

#[tokio::test]
#[ignore = "run by the parent test with an isolated process environment"]
async fn environment_key_child() {
    let client = api::Client::builder().build().unwrap();
    assert!(!format!("{client:?}").contains("environment-key"));
    client.get_dataset().send().await.unwrap();
}

#[tokio::test]
#[ignore = "run by the parent test with an isolated process environment"]
async fn explicit_key_child() {
    let client = api::Client::builder()
        .api_key("explicit-key")
        .api_url(std::env::var("TEST_API_URL").unwrap())
        .build()
        .unwrap();
    assert!(!format!("{client:?}").contains("explicit-key"));
    client.get_dataset().send().await.unwrap();
}

#[test]
#[ignore = "run by the parent test with an isolated process environment"]
fn missing_key_child() {
    assert!(matches!(
        api::Client::builder().build(),
        Err(api::ClientInitError::MissingApiKey)
    ));
}
