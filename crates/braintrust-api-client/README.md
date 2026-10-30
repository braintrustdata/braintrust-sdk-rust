# Braintrust API client

Typed async REST client generated with Progenitor 0.15.1. The main
`braintrust-sdk-rust` crate re-exports it as `braintrust_sdk_rust::api`.
Requires Rust 1.88 or newer.

```rust,no_run
use braintrust_api_client::{Client, types};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder().build()?; // Reads BRAINTRUST_API_KEY.

let body: types::CreateDataset = types::CreateDataset::builder()
    .name("example")
    .project_id("00000000-0000-0000-0000-000000000001")
    .try_into()?;
let dataset = client.post_dataset().body(body).send().await?.into_inner();
println!("{}", dataset.id);
# Ok(())
# }
```

To supply the key programmatically:

```rust,no_run
# use braintrust_api_client::Client;
# fn example() -> Result<(), braintrust_api_client::ClientInitError> {
let client = Client::builder()
    .api_key("your-api-key")
    .build()?;
# Ok(())
# }
```

Use `.api_url(url)` to override the endpoint. Otherwise the builder reads
`BRAINTRUST_API_URL`, defaulting to `https://api.braintrust.dev`.
Explicit values override environment variables.

The builder sets a sensitive bearer authorization header so Debug output does
not expose the key. Missing, empty, or invalid keys produce a `ClientInitError`.

For custom certificates, timeouts, or other transport settings, use the
generated `Client::new_with_client(url, http)` and supply your own
authenticated reqwest client. This REST client is independent of the logging
SDK's login state and batching/retry behavior.
