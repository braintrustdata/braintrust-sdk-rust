#!/usr/bin/env python3
"""Build a downstream consumer using only the two packaged crates and a remotely fetched spec."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package_directory", type=Path)
    args = parser.parse_args()
    repository = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=repository,
    ))

    def archive_for(name):
        version = next(package["version"] for package in metadata["packages"] if package["name"] == name)
        return args.package_directory / f"{name}-{version}.crate"

    sdk_archive = archive_for("braintrust-sdk-rust")
    api_archive = archive_for("braintrust-api-client")
    with tempfile.TemporaryDirectory(prefix="braintrust-packaged-client-") as directory:
        root = Path(directory)
        for archive in (sdk_archive, api_archive):
            with tarfile.open(archive) as package:
                package.extractall(root, filter="data")
        sdk = root / sdk_archive.stem
        api = root / api_archive.stem
        consumer = root / "consumer"
        (consumer / "src").mkdir(parents=True)
        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "packaged-client-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
            '\n[dependencies]\nbraintrust-sdk-rust = { path = ' + json.dumps(str(sdk)) + ' }\n'
            '\n[patch.crates-io]\nbraintrust-api-client = { path = ' + json.dumps(str(api)) + ' }\n'
        )
        (consumer / "src/main.rs").write_text('''use braintrust_sdk_rust::api;
fn main() {
    let body: api::types::CreateDataset = api::types::CreateDataset::builder()
        .name("packaged-client").project_id("00000000-0000-0000-0000-000000000001")
        .try_into().unwrap();
    let client = api::Client::new("https://api.braintrust.dev");
    let _request = client.post_dataset().body(body);
    assert!(api::OPENAPI_SPEC_SOURCE.starts_with("https://raw.githubusercontent.com/braintrustdata/braintrust-openapi/"));
    assert_eq!(api::OPENAPI_SPEC_SHA256.len(), 64);
    println!("Packaged downstream client generated from {}", api::OPENAPI_SPEC_SOURCE);
}
''')
        environment = os.environ.copy()
        # Patch only the unpublished API dependency to its unpacked crate.
        # Generation must fetch the spec without relying on source-tree files.
        environment.pop("BRAINTRUST_OPENAPI_SPEC", None)
        environment["CARGO_TARGET_DIR"] = str(repository / "target")
        subprocess.run(
            ["cargo", "run", "--manifest-path", str(consumer / "Cargo.toml")],
            env=environment, check=True,
        )


if __name__ == "__main__":
    main()
