use braintrust_sdk_rust::{configure_service, ServiceConfig, ServiceRuntime};
use std::sync::Once;

/// Each test has its own Tokio runtime, but the SDK service belongs to the
/// whole test binary. Give ordinary integration tests a runtime that lives
/// until the process exits. Runtime selection itself is tested in subprocesses.
pub fn use_dedicated_runtime() {
    static CONFIGURED: Once = Once::new();
    CONFIGURED.call_once(|| {
        configure_service(
            ServiceConfig::builder()
                .runtime(ServiceRuntime::Dedicated)
                .build(),
        )
        .expect("configure the integration test SDK runtime");
    });
}
