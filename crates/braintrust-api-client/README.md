# Braintrust API client

Typed async REST client generated with Progenitor 0.15.1. The main
`braintrust-sdk-rust` crate re-exports it as `braintrust_sdk_rust::api`.
Requires Rust 1.88 or newer.

```rust,no_run
use braintrust_api_client::{Client, types};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::new()?; // Reads BRAINTRUST_API_KEY.

let body: types::CreateDataset = types::CreateDataset::builder()
    .name("example")
    .project_id("00000000-0000-0000-0000-000000000001")
    .try_into()?;
let dataset = client.post_dataset().body(body).send().await?.into_inner();
println!("{}", dataset.id);
# Ok(())
# }
```

To supply the key programmatically, use `Client::with_api_key("your-api-key")`.
Both constructors use `BRAINTRUST_API_URL` when set, otherwise
`https://api.braintrust.dev`. For an explicit endpoint without environment
configuration, use `Client::with_api_key_and_url(key, url)`.

These constructors set the bearer authorization header and mark it sensitive
so Debug output does not expose the key. Missing, empty, or invalid keys produce
a `ClientInitError`.

For custom certificates, timeouts, or other transport settings, use the
unwrapped `GeneratedClient::new_with_client(url, http)` and supply your own
authenticated reqwest client. This REST client is independent of the logging
SDK's login state and batching/retry behavior.

## Generation during builds

The crate's `build.rs` generates Rust from the checked-in `openapi.json` into
Cargo's `OUT_DIR`, including when another project builds it as a dependency.
Builds never download a spec. Java, Node, and a separate generator executable
are not required. Progenitor and its runtime are pinned.

Cargo tracks changes to the snapshot, its provenance, and the adaptation code.
`OPENAPI_SPEC_SOURCE` and `OPENAPI_SPEC_SHA256` identify the unmodified input
used by a compiled client. The build verifies the digest in
`openapi-source.json` before generating code.

The crate includes both snapshot files, so generation works offline and on
docs.rs once Cargo dependencies are available. The API crate is versioned
independently of the upstream spec.

## Automated snapshot updates

After a commit lands on `braintrust-openapi` main, its dispatcher sends a
`repository_dispatch` event with type `braintrust-openapi-updated`. The
`.github/workflows/update-api.yml` receiver accepts this payload:

```json
{
  "spec_sha": "<full published commit SHA>"
}
```

The source repository is hard-coded to `braintrustdata/braintrust-openapi`.
The receiver verifies the notified commit is on upstream main, coalesces delayed
notifications to the latest published main revision, and downloads that exact
revision. If the spec bytes changed, it updates `openapi.json` and
`openapi-source.json` on a persistent `chore/update-openapi-snapshot` PR. API
crate versions are not changed automatically. SDK CI builds from the proposed
snapshot and validates the generated API before merge.

Notifications that do not change the spec content create no new PR. If an
upstream change reverts to the SDK snapshot, an obsolete pending update PR is
closed. Serialized runs
and commit ancestry checks prevent older notifications from downgrading an
update. A manual `workflow_dispatch` with the same inputs can retry an update.

The receiver needs `GH_BOT_APP_PRIVATE_KEY` for the Braintrust bot installed on
this SDK repository, with Contents and Pull requests write access. Its public
App ID defaults to `1394756`; set the repository variable `GH_BOT_APP_ID` to
use another app. The bot token ensures update PRs trigger ordinary SDK CI.

## Spec adaptations and coverage

`build_support/adapt.rs` adapts the snapshot before generation:

- Represent `type: null` schemas as null-only enums.
- Remove inline schema titles so type names derive from their context, while
  preserving component annotations and payload fields named `title`.
- Give nullable string component enums separate non-null enum components to
  avoid colliding enum/wrapper names.
- Exclude browser CORS `OPTIONS` operations, for which Progenitor emits an
  unsupported reqwest method.
- Exclude `/v1/proxy/{path+}`, whose array-valued catch-all path parameter is
  unsupported by Progenitor's path renderer. Concrete proxy routes remain.

Existing logging, login, and other SDK endpoints absent from the public spec
remain handwritten.

## Publishing

Publish `braintrust-api-client` before the main SDK, which depends on its exact
version. The published API crate includes its build script, adaptations, and synced
snapshot, so downstream generation needs neither repository-only files nor a
network request.
