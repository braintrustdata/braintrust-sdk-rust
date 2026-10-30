mod adapt;

use openapi_to_rust::{CodeGenerator, GeneratorConfig, SchemaAnalyzer};
use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let input = PathBuf::from(
        args.next()
            .ok_or("usage: generate-api SPEC.json OUTPUT_DIRECTORY")?,
    );
    let output = PathBuf::from(
        args.next()
            .ok_or("usage: generate-api SPEC.json OUTPUT_DIRECTORY")?,
    );
    if args.next().is_some() {
        return Err("usage: generate-api SPEC.json OUTPUT_DIRECTORY".into());
    }
    let mut spec = serde_json::from_slice(&fs::read(input)?)?;
    let adaptations = adapt::adapt_spec(&mut spec)?;
    eprintln!("Simplified {adaptations} redundant nullable intersections");
    let mut analysis = SchemaAnalyzer::new(spec)?.analyze()?;
    let config = GeneratorConfig {
        // Fixed provenance avoids timestamps and temporary input paths in diffs.
        spec_path:
            "https://github.com/braintrustdata/braintrust-openapi/blob/main/openapi/spec.json"
                .into(),
        enable_sse_client: false,
        tracing_enabled: false,
        builders: openapi_to_rust::config::BuildersSection {
            enabled: true,
            threshold: 0,
        },
        ..GeneratorConfig::default()
    };
    let generated = CodeGenerator::new(config).generate_all(&mut analysis)?;
    fs::create_dir_all(&output)?;
    for file in generated
        .files
        .iter()
        .chain(std::iter::once(&generated.mod_file))
    {
        if !matches!(
            file.path.to_str(),
            Some("client.rs" | "types.rs" | "mod.rs")
        ) {
            return Err(format!("unexpected generated file: {}", file.path.display()).into());
        }
        fs::write(output.join(&file.path), &file.content)?;
    }
    Ok(())
}
