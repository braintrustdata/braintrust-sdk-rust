# Braintrust API client

Async Rust client for managing Braintrust resources, querying data, and writing
logs. Use it directly or through `braintrust_sdk_rust::api`.
Requires Rust 1.88 or newer.

## Create a dataset

Set `BRAINTRUST_API_KEY` and provide your project ID:

```rust,no_run
use braintrust_api_client::{Client, types};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder().build()?;
let body: types::CreateDataset = types::CreateDataset::builder()
    .name("example")
    .project_id("00000000-0000-0000-0000-000000000001")
    .try_into()?;
let dataset = client.post_dataset().body(body).send().await?.into_inner();
println!("{}", dataset.id);
# Ok(())
# }
```

To provide a key in code, use `Client::builder().api_key("your-api-key").build()?`.

## Connect to an organization

Use login to find your organization's API endpoint automatically:

```rust,no_run
# use braintrust_api_client::Client;
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder()
    .org_name("my-org")
    .build_with_login()
    .await?;
# Ok(())
# }
```

Omit `.org_name(...)` to select the first available organization. To list your
organizations, call `client.login().await?`.

## Query data and write logs

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

Use `btql()` for JSONL or Parquet results, and `logs3_raw()` when your log payload
is already serialized. For buffered background logging, use the main SDK's
logging APIs.

## Customize your connection

Use `.api_url(url)` for a custom API endpoint and `.app_url(url)` for a custom
login endpoint. These also read `BRAINTRUST_API_URL` and `BRAINTRUST_APP_URL`,
respectively. Explicit settings take precedence over environment variables.
Defaults are `https://api.braintrust.dev` and `https://www.braintrust.dev`.

Configure timeouts, proxies, or trusted certificates with a reqwest builder:

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

For custom settings when uploading large log batches to storage, use
`.upload_http_client_builder(...)`.
