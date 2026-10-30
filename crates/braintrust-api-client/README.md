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

For custom certificates, proxies, timeouts, or other transport settings, supply
a reqwest builder. Its settings are preserved, with bearer authentication added:

```rust,no_run
# use braintrust_api_client::Client;
# fn example() -> Result<(), braintrust_api_client::ClientInitError> {
let client = Client::builder()
    .http_client_builder(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30)))
    .build()?;
# Ok(())
# }
```

The configured API key overrides any default authorization header on the reqwest
builder. This REST client is independent of the logging SDK's login state and
batching/retry behavior.

## Login and organization discovery

`build()` makes no network requests. To discover an organization's API endpoint:

```rust,no_run
# use braintrust_api_client::Client;
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder()
    .org_name("my-org")
    .build_with_login()
    .await?;
println!("{}", client.organization().unwrap().id);
# Ok(())
# }
```

Login uses `.app_url(url)`, or `BRAINTRUST_APP_URL`, defaulting to
`https://www.braintrust.dev`. It selects the first organization when no name is
provided. Its discovered API URL replaces the configured fallback; if absent,
the fallback remains. `client.login()` lists organizations without changing the
client's endpoint. Organization selection also sets the `x-bt-org-name` header.

## Queries and logging

The same client supports endpoints outside the public REST specification:

```rust,no_run
# use braintrust_api_client::Client;
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder().build()?;
let result = client.btql_json(&serde_json::json!({
    "query": "select * from project_logs('project-id') limit 10"
})).await?;
println!("{:?}", result.data);

client.logs3(&serde_json::json!({
    "rows": [{"project_id": "project-id", "id": "event-id", "input": "hello"}],
    "api_version": 2
})).await?;
# Ok(())
# }
```

`btql()` returns the raw response for JSONL or Parquet queries. `logs3_raw()`
accepts already-serialized JSON. `version()` reports server capabilities.
These methods make individual requests; batching and retry policy belong to
the caller. `control_plane(object, action, &body)` supports legacy app operations;
`experiment_comparison()` and `dataset_summary()` use the API endpoint.

For oversized logs, request a destination with `logs3_overflow()`, upload with
`upload_logs3_overflow()`, then submit its key as a `logs3_overflow` reference
through `logs3()`. Signed POST and PUT uploads use a separate transport without
Braintrust credentials. Configure its certificates, proxies, or timeouts using
`.upload_http_client_builder(...)` when needed; API transport settings are not
copied to storage requests.

Handwritten operations use the same `Error` and `ResponseValue` types as the
REST methods. Unsuccessful HTTP responses are preserved in
`Error::UnexpectedResponse` for inspecting their status and body.
