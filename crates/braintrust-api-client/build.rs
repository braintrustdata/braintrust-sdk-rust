#[path = "build_support/adapt.rs"]
mod adapt;

use std::{env, error::Error, fs, io::Read, path::PathBuf, time::Duration};

use sha2::{Digest, Sha256};

const SNAPSHOT: &[u8] = include_bytes!("openapi.json");
const SNAPSHOT_SOURCE: &str = "bundled braintrust-openapi@90ef74e4573fccd223a3c365bba329bff19aa362";
const MAX_SPEC_BYTES: u64 = 8 * 1024 * 1024;

fn main() {
    if let Err(error) = generate() {
        panic!("Braintrust API client generation failed: {error}");
    }
}

fn generate() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=openapi.json");
    println!("cargo:rerun-if-changed=build_support");
    for name in [
        "BRAINTRUST_OPENAPI_SPEC",
        "BRAINTRUST_OPENAPI_REF",
        "BRAINTRUST_OPENAPI_OFFLINE",
        "BRAINTRUST_OPENAPI_REQUIRE_REMOTE",
        "DOCS_RS",
        "CARGO_NET_OFFLINE",
        "BRAINTRUST_OPENAPI_REFRESH",
        "BRAINTRUST_CUSTOM_CA_BUNDLE",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let (bytes, source) = load_spec()?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let mut spec: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid JSON in {source}: {error}"))?;
    let changes =
        adapt::adapt_spec(&mut spec).map_err(|error| format!("cannot adapt {source}: {error}"))?;
    println!("Braintrust OpenAPI: {source} (sha256 {digest}); {changes:?}");

    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    fs::write(out.join("openapi.original.json"), &bytes)?;
    fs::write(
        out.join("openapi.adapted.json"),
        serde_json::to_vec_pretty(&spec)?,
    )?;
    let api = serde_json::from_value(spec)
        .map_err(|error| format!("adapted spec is not supported by Progenitor: {error}"))?;
    let mut settings = progenitor::GenerationSettings::default();
    settings.with_interface(progenitor::InterfaceStyle::Builder);
    let tokens = progenitor::Generator::new(&settings).generate_tokens(&api)?;
    let syntax = syn::parse2(tokens)?;
    fs::write(out.join("api.rs"), prettyplease::unparse(&syntax))?;
    println!("cargo:rustc-env=BRAINTRUST_GENERATED_SPEC_SOURCE={source}");
    println!("cargo:rustc-env=BRAINTRUST_GENERATED_SPEC_SHA256={digest}");
    Ok(())
}

fn enabled(name: &str) -> bool {
    env::var(name).is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn load_spec() -> Result<(Vec<u8>, String), Box<dyn Error>> {
    let required = enabled("BRAINTRUST_OPENAPI_REQUIRE_REMOTE");
    if let Some(path) = env::var_os("BRAINTRUST_OPENAPI_SPEC") {
        if required {
            return Err(
                "BRAINTRUST_OPENAPI_SPEC conflicts with BRAINTRUST_OPENAPI_REQUIRE_REMOTE".into(),
            );
        }
        // Relative paths are relative to the API crate, even downstream. Use
        // absolute paths in CI or a consuming application's configuration.
        let path = PathBuf::from(path).canonicalize()?;
        println!("cargo:rerun-if-changed={}", path.display());
        return Ok((fs::read(&path)?, path.display().to_string()));
    }
    if enabled("BRAINTRUST_OPENAPI_OFFLINE")
        || enabled("CARGO_NET_OFFLINE")
        || env::var_os("DOCS_RS").is_some()
    {
        if required {
            return Err(
                "offline generation conflicts with BRAINTRUST_OPENAPI_REQUIRE_REMOTE".into(),
            );
        }
        return Ok((SNAPSHOT.to_vec(), SNAPSHOT_SOURCE.into()));
    }
    let reference = env::var("BRAINTRUST_OPENAPI_REF").unwrap_or_else(|_| "main".into());
    if reference.is_empty() || reference.contains(['?', '#', '\n', '\r']) {
        return Err(
            "BRAINTRUST_OPENAPI_REF must be a Git ref without query or fragment characters".into(),
        );
    }
    let url = format!(
        "https://raw.githubusercontent.com/braintrustdata/braintrust-openapi/{reference}/openapi/spec.json"
    );
    match fetch_spec(&url) {
        Ok(bytes) => Ok((bytes, url)),
        Err(error) if !required => {
            println!(
                "cargo:warning=Could not fetch Braintrust OpenAPI spec ({error}); using {SNAPSHOT_SOURCE}. Set BRAINTRUST_OPENAPI_REQUIRE_REMOTE=1 to fail instead."
            );
            Ok((SNAPSHOT.to_vec(), SNAPSHOT_SOURCE.into()))
        }
        Err(error) => Err(format!("could not fetch {url}: {error}").into()),
    }
}

fn fetch_spec(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!(
            "braintrust-api-client-build/",
            env!("CARGO_PKG_VERSION")
        ));
    if let Ok(pem) = env::var("BRAINTRUST_CUSTOM_CA_BUNDLE") {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())?;
        if certs.is_empty() {
            return Err("BRAINTRUST_CUSTOM_CA_BUNDLE did not contain any PEM certificates".into());
        }
        for certificate in certs {
            builder = builder.add_root_certificate(certificate);
        }
    }
    let client = builder.build()?;
    let response = client.get(url).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAX_SPEC_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SPEC_BYTES {
        return Err("OpenAPI spec exceeded the 8 MiB limit".into());
    }
    Ok(bytes)
}
