#[path = "build_support/adapt.rs"]
mod adapt;

use std::{env, error::Error, fs, path::PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    if let Err(error) = generate() {
        panic!("Braintrust API client generation failed: {error}");
    }
}

fn generate() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build_support");
    println!("cargo:rerun-if-changed=openapi.json");
    println!("cargo:rerun-if-changed=openapi-source.json");
    let bytes: &[u8] = include_bytes!("openapi.json");
    let metadata: serde_json::Value = serde_json::from_str(include_str!("openapi-source.json"))?;
    let commit = metadata["commit"]
        .as_str()
        .ok_or("snapshot source commit is missing")?;
    let source = format!("bundled braintrust-openapi@{commit}");
    let digest = format!("{:x}", Sha256::digest(bytes));
    if metadata["repository"] != "braintrustdata/braintrust-openapi" || metadata["sha256"] != digest
    {
        return Err("bundled OpenAPI spec does not match openapi-source.json".into());
    }
    let mut spec: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid JSON in {source}: {error}"))?;
    let changes =
        adapt::adapt_spec(&mut spec).map_err(|error| format!("cannot adapt {source}: {error}"))?;
    println!("Braintrust OpenAPI: {source} (sha256 {digest}); {changes:?}");

    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    fs::write(out.join("openapi.original.json"), bytes)?;
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
