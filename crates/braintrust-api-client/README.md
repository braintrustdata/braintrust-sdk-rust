# Braintrust API client

Typed async REST client generated with Progenitor 0.15.1. The main
`braintrust-sdk-rust` crate re-exports it as `braintrust_sdk_rust::api`.
Requires Rust 1.88 or newer.

```rust,no_run
use braintrust_api_client::{Client, types};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut headers = reqwest::header::HeaderMap::new();
let mut authorization: reqwest::header::HeaderValue =
    format!("Bearer {}", std::env::var("BRAINTRUST_API_KEY")?).parse()?;
authorization.set_sensitive(true);
headers.insert(reqwest::header::AUTHORIZATION, authorization);
let http = reqwest::Client::builder().default_headers(headers).build()?;
let client = Client::new_with_client("https://api.braintrust.dev", http);

let body: types::CreateDataset = types::CreateDataset::builder()
    .name("example")
    .project_id("00000000-0000-0000-0000-000000000001")
    .try_into()?;
let dataset = client.post_dataset().body(body).send().await?.into_inner();
println!("{}", dataset.id);
# Ok(())
# }
```

Supply your own reqwest client for authentication, custom certificates,
timeouts, or other transport settings. This REST client is independent of the
logging SDK's login state and batching/retry behavior.

## Generation during builds

The crate's `build.rs` downloads
<https://raw.githubusercontent.com/braintrustdata/braintrust-openapi/main/openapi/spec.json>,
adapts it, and generates Rust in Cargo's `OUT_DIR`. This also happens when the
crate is built as a dependency of another project. Java, Node, and a separate
generator executable are not required. Progenitor and its runtime are pinned.

Cargo caches successful build-script output. A normal incremental build may
reuse the previous client. Changing `BRAINTRUST_OPENAPI_REFRESH` to any new
value requests another fetch and generation; CI sets it to the workflow run.

The raw and adapted specs are saved alongside `api.rs` in `OUT_DIR` for
inspection. `OPENAPI_SPEC_SOURCE` and `OPENAPI_SPEC_SHA256` identify the input
used by a compiled client. The digest covers the unmodified input.

## Selecting the spec

| Environment variable | Behavior |
| --- | --- |
| `BRAINTRUST_OPENAPI_REF` | Git ref to fetch; defaults to `main`. Set a commit SHA to reproduce a build. |
| `BRAINTRUST_OPENAPI_SPEC` | Use a local JSON spec instead of fetching. Prefer an absolute path; relative paths are resolved from this crate's directory. Cargo tracks changes to the file. |
| `BRAINTRUST_OPENAPI_OFFLINE=1` | Generate from the bundled snapshot without fetching. |
| `CARGO_NET_OFFLINE=true` | Also selects the bundled snapshot. Cargo's `--offline` flag alone does not communicate this setting to build scripts; set this environment variable or the one above. |
| `BRAINTRUST_OPENAPI_REQUIRE_REMOTE=1` | Fail if fetching fails. Conflicts with local/offline selection. CI enables this. |
| `BRAINTRUST_OPENAPI_REFRESH` | Change this value to invalidate Cargo's cached generation. |
| `BRAINTRUST_CUSTOM_CA_BUNDLE` | Additional PEM CA certificates for the build-time download. Standard proxy environment variables also apply. |

`1` and `true` enable boolean options. Without strict remote mode, download
failures produce a Cargo warning and generation uses the bundled snapshot.
Invalid downloaded JSON, unsupported specs, and generation failures always fail
the build; they do not silently fall back. docs.rs builds use the bundled
snapshot because their build environment blocks network access.

The bundled `openapi.json` is the unmodified public spec from
`braintrust-openapi@90ef74e4573fccd223a3c365bba329bff19aa362`, SHA-256
`6409b3f2bde596c05fd73c229bb1e13e42c5e1b77f434eeb2b792fbaef96b720`.
Refresh it from a known public commit when updating the fallback.

Fetching `main` means the generated API can change without changing this
crate's version or Cargo.lock. Use a fixed ref or local spec when repeatability
matters. New backend schema shapes can require changes to the adaptations or
declared dependencies before the crate can build against them.

## Spec adaptations and coverage

`build_support/adapt.rs` applies the same rules to remote, local, and bundled
specs before generation:

- Represent `type: null` schemas as null-only enums.
- Remove inline schema titles so type names derive from their context, while
  preserving component annotations and payload fields named `title`.
- Give nullable string component enums separate non-null enum components to
  avoid colliding enum/wrapper names.
- Exclude browser CORS `OPTIONS` operations, for which Progenitor emits an
  unsupported reqwest method.
- Exclude `/v1/proxy/{path+}`, whose array-valued catch-all path parameter is
  unsupported by Progenitor's path renderer. Concrete proxy routes remain.

The bundled spec yields 168 operations after these exclusions. Existing logging,
login, and other SDK endpoints absent from the public spec remain handwritten.

## Publishing

Publish `braintrust-api-client` before the main SDK, which depends on its exact
version. The published API crate includes its build script, adaptations, and
snapshot, so downstream generation does not depend on repository-only files.
