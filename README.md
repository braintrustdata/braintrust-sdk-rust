[![Braintrust](./braintrust-logo.svg)](https://www.braintrust.dev/)

# braintrust-sdk-rust

Rust SDK for [Braintrust](https://braintrust.dev) logging and tracing.

> **Early Development Notice**: This SDK is in early development (alpha). Expect backwards-incompatible changes between releases until we reach 1.0.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
braintrust-sdk-rust = "0.1.0-alpha.1"
```

## Usage

```rust
use braintrust_sdk_rust::{BraintrustClient, BraintrustClientConfig, SpanLog};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create a client pointing to your Braintrust instance
    let client = BraintrustClient::new(
        BraintrustClientConfig::new("https://api.braintrust.dev")
    )?;

    // Create a span for logging
    let span = client
        .span_builder("your-api-token", "your-org-id")
        .project_name("my-project")
        .build();

    // Log input/output using span.log() with the builder pattern
    span.log(
        SpanLog::builder()
            .input(json!({"prompt": "Hello, world!"}))
            .output(json!({"response": "Hi there!"}))
            .build()
            .expect("Failed to build SpanLog")
    ).await;

    // Flush the span data to Braintrust
    span.flush().await?;
    client.flush().await?;

    Ok(())
}
```

## Features

- **Span-based logging**: Create spans to track LLM calls and other operations
- **Usage metrics extraction**: Built-in extractors for OpenAI and Anthropic usage metrics
- **Async-first**: Built on Tokio for high-performance async operations
- **Background submission**: Logs are submitted in the background to minimize latency

## Generated REST API client

The SDK includes a typed REST API client at `braintrust_sdk_rust::api`.
See the [API client README](crates/braintrust-api-client/README.md) for usage.

## Preserving existing span provenance

For updates that must not replace an existing span's origin, build the update
with `SpanLog::builder().skip_span_origin()`. This omits automatically generated
`context.span_origin` fields, including configured SDK origin and environment
overrides. Explicit or propagated context remains unchanged; callers preserving
an existing origin should not supply a replacement in that context.

When logged to a `SpanHandle`, the option remains enabled through subsequent
logs and `end()`. It does not delete provenance already stored on the backend.

## Private CA bundles

When outbound HTTPS traffic passes through a TLS-inspecting proxy, set
`BRAINTRUST_CUSTOM_CA_BUNDLE` to the PEM contents of the additional CA
certificate bundle. The SDK appends these certificates to its normal public
trust store and continues to use the standard `HTTP_PROXY`, `HTTPS_PROXY`, and
`NO_PROXY` behavior.

For programmatic configuration, use
`BraintrustClient::builder().custom_ca_bundle_pem(pem)`. The existing
`.ca_bundle(path)` method remains available when the PEM bundle is stored in a
file.

## Extracting Usage Metrics

The SDK includes helpers for extracting usage metrics from provider responses:

```rust
use braintrust_sdk_rust::{extract_openai_usage, extract_anthropic_usage};
use serde_json::json;

// Extract from OpenAI response
let openai_response = json!({
    "usage": {
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "total_tokens": 150
    }
});
let usage = extract_openai_usage(&openai_response);

// Extract from Anthropic response
let anthropic_response = json!({
    "usage": {
        "input_tokens": 100,
        "output_tokens": 50
    }
});
let usage = extract_anthropic_usage(&anthropic_response);
```

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).

### Sharing clients across projects and organizations

SDK clients and project handles share one background delivery service and compatible HTTP connection pools. Creating another project handle does not create another queue or worker. Each handle retains its own routing context; a project's flush waits for its accepted work, and its parent client's flush includes work submitted through derived project handles.

```rust,no_run
# use braintrust_sdk_rust::BraintrustClient;
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = BraintrustClient::builder().api_key("your-key").build().await?;
let traces = client.project("gateway-traces");
let evaluations = client.project("evaluation-traces");
let span = traces.span_builder().await?.build();
span.end();
traces.flush().await?;
client.flush().await?;

// Selection succeeds only if these credentials can access the organization.
// Existing handles keep their original organization and endpoint.
let other_org = client.with_organization("another-org").await?;
# Ok(())
# }
```

For different credentials, construct another client normally. `build_without_login()` also works without a caller Tokio runtime; its per-call credential methods route uploads through the same shared service. Token providers execute on the SDK runtime, so their implementations must not depend on a caller runtime remaining alive.

Process-wide, per-org, and per-destination admission limits are optional. Unresolved project names and explicit IDs have separate admission identities. Configure them before constructing the first client or bulk uploader; conflicting later configuration returns an error. Per-client queue limits always apply, and retries remain counted against retained work while releasing active HTTP request capacity.

```rust,no_run
use braintrust_sdk_rust::{configure_service, ServiceConfig};

configure_service(ServiceConfig::builder()
    .max_concurrent_requests(8)
    .max_rows(100_000)
    .max_bytes(64 * 1024 * 1024)
    .build())?;
# Ok::<(), braintrust_sdk_rust::BraintrustError>(())
```

SDK-issued operations return eager `Completion` values: calling the method starts the operation, `.await` observes it in async code, and `.blocking_wait()` observes it in synchronous code. Dropping a completion leaves accepted work running. Pending blocking waits are refused inside Tokio runtime contexts. The standalone `braintrust-api-client` crate retains its async, one-attempt transport interface. The `Task`, `Scorer`, evaluation `Dataset`, and `TokenProvider` extension traits retain their existing async contracts, including support for borrowed inputs and implementations.

```rust,no_run
use braintrust_sdk_rust::BraintrustClient;

let client = BraintrustClient::builder().api_key("your-key").build().blocking_wait()?;
let span = client.project("gateway-traces").span_builder().blocking_wait()?.build();
span.log(Default::default()).blocking_wait()?;
client.flush().blocking_wait()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Delivery runs automatically with bounded admission. Write completions report admission and delivery failures, and flush completions report failures for work pending at their cutoff. Terminal failures do not poison subsequent flushes. `dropped_rows()` reports losses for the handle and its derived handles. Call `flush()` or `shutdown(timeout)` and observe its completion before exiting when delivery matters. Shutdown closes only the selected handle and its descendants; other clients and sibling projects continue using the shared service.
