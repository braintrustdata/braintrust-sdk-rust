#[path = "build_support/adapt.rs"]
mod adapt;

use std::{env, error::Error, fs, path::PathBuf};

fn main() {
    if let Err(error) = generate() {
        panic!("Braintrust API client generation failed: {error}");
    }
}

fn generate() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build_support");
    println!("cargo:rerun-if-changed=openapi.json");
    let bytes: &[u8] = include_bytes!("openapi.json");
    let mut spec: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid JSON in openapi.json: {error}"))?;
    let changes = adapt::adapt_spec(&mut spec)
        .map_err(|error| format!("cannot adapt openapi.json: {error}"))?;
    println!("Braintrust OpenAPI adaptations: {changes:?}");

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
    Ok(())
}
