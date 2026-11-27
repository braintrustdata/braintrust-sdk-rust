use braintrust_sdk_rust::{configure_service, ServiceConfig, ServiceRuntime};

/// Each test has its own Tokio runtime, but the SDK service belongs to the
/// whole test binary. Give ordinary integration tests a runtime that lives
/// until the process exits. Runtime selection itself is tested in subprocesses.
pub fn use_dedicated_runtime() {
    configure_service(
        ServiceConfig::builder()
            .runtime(ServiceRuntime::Dedicated)
            .build(),
    )
    .expect("configure the integration test SDK runtime");
}

/// Start the mock API after configuring the process-wide SDK runtime.
pub async fn mock_server() -> wiremock::MockServer {
    use_dedicated_runtime();
    wiremock::MockServer::start().await
}
