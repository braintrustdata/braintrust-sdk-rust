[![Braintrust](https://raw.githubusercontent.com/braintrustdata/braintrust-sdk-rust/main/braintrust-logo.svg)](https://www.braintrust.dev/)

# braintrust-sdk-rust

Rust SDK for [Braintrust](https://braintrust.dev) logging and tracing.

> **Early Development Notice**: This SDK is in early development (alpha). Expect backwards-incompatible changes between releases until we reach 1.0.

## Installation

The API below is unreleased. To use this checkout, add it as a path dependency:

```toml
[dependencies]
braintrust-sdk-rust = { path = "../braintrust-sdk-rust" }
```

## Rust version

The minimum supported Rust version is **1.86.0**. This repository uses Cargo
resolver 3 to prefer dependency versions compatible with its declared MSRV.

Consuming applications control their own resolver and lockfile. For Rust 1.86
builds, select resolver 3 in the application's root manifest, for example:

```toml
[workspace]
resolver = "3"
```

Regenerate an existing incompatible lockfile with the compatible resolver;
changing the resolver alone does not replace versions already locked. See
[Cargo's dependency resolver documentation](https://doc.rust-lang.org/cargo/reference/resolver.html#resolver-versions).

## Logging and tracing

Set `BRAINTRUST_API_KEY`, or pass a key with `.api_key(...)`. A client owns its
runtime by default, including when built inside an existing Tokio runtime.

```rust,no_run
use braintrust_sdk_rust::{BraintrustClient, SpanLog};
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = BraintrustClient::builder()
        .default_project("my-project")
        .build()
        .await?;

    let span = client.span_builder().await?.name("greeting").build();
    let write = span.log(
        SpanLog::builder()
            .input(json!({"prompt": "Hello, world!"}))
            .output(json!({"response": "Hi there!"}))
            .build()?,
    );
    write.await?;
    span.end().await?;
    client.shutdown(Duration::from_secs(10)).await?;
    Ok(())
}
```

`log()` submits immediately. `end()` finishes the span and returns a completion
for delivery of its final snapshot. Later mutations of a finished span are
ignored with a diagnostic. Create child spans for separate operations; use
`update_span()` or `log_feedback()` to patch an existing row explicitly.

## Submission and completion

Methods that schedule work return `Completion<T, E>`. Calling the method submits
the operation; awaiting its token observes the result. Inspect `&*completion`
for the immediate admission `Result<(), E>`, such as a full or closed queue.
An admitted operation can still fail later. Dropping the token leaves accepted
work running.

```rust,no_run
use braintrust_sdk_rust::{SpanHandle, SpanLog};

async fn log(span: &SpanHandle) -> anyhow::Result<()> {
    let completion = span.log(SpanLog::default());
    if let Err(error) = &*completion {
        eprintln!("submission rejected: {error}");
    }
    completion.await?;
    Ok(())
}
```

Writes are batched and submitted automatically. `client.flush()` captures queued
and active writes through the point of the call and observes their delivery.
A terminal batch failure fails its write completions and covering flush, then
retires that batch. Later flushes process new work independently. Failures are
not retained for replay.

Await `client.shutdown(timeout)` before exiting to close admission and drain
accepted work. A timeout fails remaining completions and discards pending SDK
state. Concurrent shutdown calls share the first call's deadline and outcome.

## Synchronous applications and runtime selection

The same API works on a plain thread without creating a Tokio runtime. Use a
future executor to wait for values when needed; for example, with the `futures`
crate:

```rust,no_run
use braintrust_sdk_rust::BraintrustClient;
use std::{future::IntoFuture, time::Duration};

fn main() -> anyhow::Result<()> {
    let client = futures::executor::block_on(
        BraintrustClient::builder().build().into_future(),
    )?;
    futures::executor::block_on(client.shutdown(Duration::from_secs(10)).into_future())?;
    Ok(())
}
```

To select your own Tokio runtime, pass `.runtime(current_runtime())` while
inside it, or `.runtime(Some(handle))`. With an explicit handle, keep that
runtime alive and driving work until completions finish. `current_runtime()`
returns `None` outside Tokio, which selects an SDK-owned runtime.

## Evaluations

An `EvalDefinition` contains reusable task and scorer callbacks. Each run supplies
its own case source and options. Local runs need no client, credentials or
serialization:

```rust
use braintrust_sdk_rust::eval::{
    self, Case, EvalDefinition, EvalOpts, EvalRequest, ExactMatch, SyncFnTask,
};

fn main() -> anyhow::Result<()> {
    let definition = EvalDefinition::new(
        Box::new(SyncFnTask::new(|input: &String, _hooks| Ok(input.to_uppercase()))),
        vec![Box::new(ExactMatch::new())],
    )?;
    let options = EvalOpts::builder()
        .name("uppercase".into())
        .definition(definition)
        .source(vec![Case {
            input: "hello".to_string(),
            expected: Some("HELLO".to_string()),
            ..Default::default()
        }].into())
        .quiet(true)
        .build();
    let completion = eval::run(EvalRequest::local(options));
    assert!(completion.is_ok());
    let summary = futures::executor::block_on(async { completion.await })?;
    assert_eq!(summary.successful_cases, 1);
    assert_eq!(summary.results[0].output.as_deref(), Some("HELLO"));
    Ok(())
}
```

Use `EvalRequest::recorded(options, client)` to record an experiment on that
client's runtime. Recorded inputs and outputs must implement `Serialize`.
`DatasetCaseSource` reads Braintrust datasets and preserves dataset provenance
when recording. Run failures retain partial summaries and structured causes.

Use `task(...)` and `scorer(...)` for async callbacks, or `SyncFnTask` and
`SyncFnScorer` for blocking callbacks. Attach a terminal, JSONL, silent or custom
reporter with `EvalRequest::with_reporter(...)`. Reporter failures preserve
computation and recording results.

`BtEvalRunner` adapts the same execution engine to `bt eval`, including discovery,
filters and output transports. See the
[CLI example](https://github.com/braintrustdata/braintrust-sdk-rust/blob/main/examples/eval_async.rs)
and [synchronous example](https://github.com/braintrustdata/braintrust-sdk-rust/blob/main/examples/eval_sync.rs).

## API organization

- `BraintrustClient` creates concrete dataset, experiment and span handles with
  shared credentials and lifecycle.
- `eval` separates definitions, case sources, execution, recording and reporting.
- `api::Client` exposes direct endpoint operations with one HTTP attempt per
  call. Use it when your application owns delivery and retry policy.
- Usage extractors support OpenAI and Anthropic responses; streaming helpers
  support provider response assembly.

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

This project is licensed under the [Apache License, Version 2.0](https://github.com/braintrustdata/braintrust-sdk-rust/blob/main/LICENSE).
