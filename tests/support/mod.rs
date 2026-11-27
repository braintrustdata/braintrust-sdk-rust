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

/// A Wiremock server whose startup configures the integration test SDK runtime.
pub struct MockServer(wiremock::MockServer);

impl MockServer {
    pub async fn start() -> Self {
        use_dedicated_runtime();
        Self(wiremock::MockServer::start().await)
    }
}

impl std::ops::Deref for MockServer {
    type Target = wiremock::MockServer;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
