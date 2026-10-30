#!/usr/bin/env python3
"""Update the SDK's OpenAPI snapshot from an immutable, published upstream commit."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import urllib.request

REPOSITORY = "braintrustdata/braintrust-openapi"
MAX_BYTES = 8 * 1024 * 1024


def fetch_bytes(url):
    headers = {"User-Agent": "braintrust-sdk-rust-spec-update"}
    # Authenticate API requests only; never send a token to the raw-content host.
    if url.startswith("https://api.github.com/") and os.environ.get("GH_TOKEN"):
        headers["Authorization"] = f"Bearer {os.environ['GH_TOKEN']}"
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=30) as response:
        data = response.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("OpenAPI response exceeded the 8 MiB limit")
    return data


def update(root, spec_sha, repository=REPOSITORY, contract_version="1", fetch=fetch_bytes):
    if contract_version != "1" or repository != REPOSITORY:
        raise ValueError("unsupported spec update contract or source repository")
    if not re.fullmatch(r"[0-9a-f]{40}", spec_sha):
        raise ValueError("spec_sha must be a full lowercase commit SHA")
    api = f"https://api.github.com/repos/{REPOSITORY}"
    latest = json.loads(fetch(f"{api}/commits/main"))["sha"]
    published = json.loads(fetch(f"{api}/compare/{spec_sha}...{latest}"))
    if published["status"] not in ("ahead", "identical"):
        raise ValueError("the requested spec commit has not landed on upstream main")

    # Coalesce delayed notifications to the current immutable main revision.
    # Combined with serialized receiver runs, this prevents an old event from
    # replacing an open update PR with an older snapshot.
    spec_sha = latest
    directory = root / "crates/braintrust-api-client"
    snapshot = directory / "openapi.json"
    provenance = directory / "openapi-source.json"
    if provenance.exists():
        previous = json.loads(provenance.read_text())
        if previous["repository"] != REPOSITORY:
            raise ValueError("existing snapshot has an unexpected source repository")
        old_sha = previous["commit"]
        if not re.fullmatch(r"[0-9a-f]{40}", old_sha):
            raise ValueError("existing snapshot source commit is invalid")
        relationship = json.loads(fetch(f"{api}/compare/{old_sha}...{spec_sha}"))["status"]
        if relationship == "behind":
            print("Ignoring an older spec update; the SDK already has a newer revision")
            return False
        if relationship not in ("ahead", "identical"):
            raise ValueError("the requested spec commit diverges from the SDK's current source")

    data = fetch(f"https://raw.githubusercontent.com/{REPOSITORY}/{spec_sha}/openapi/spec.json")
    spec = json.loads(data)
    if not isinstance(spec, dict) or not str(spec.get("openapi", "")).startswith("3.0."):
        raise ValueError("expected an OpenAPI 3.0 spec for Progenitor")
    if not isinstance(spec.get("paths"), dict) or not spec["paths"]:
        raise ValueError("OpenAPI spec has no paths")
    digest = hashlib.sha256(data).hexdigest()
    if snapshot.exists() and snapshot.read_bytes() == data:
        if provenance.exists() and previous.get("sha256") == digest:
            print("Spec content is unchanged; no update PR is needed")
            return False
    directory.mkdir(parents=True, exist_ok=True)
    snapshot.write_bytes(data)
    provenance.write_text(json.dumps({
        "repository": REPOSITORY, "commit": spec_sha, "sha256": digest,
    }, indent=2) + "\n")
    print(f"Updated snapshot from {REPOSITORY}@{spec_sha} (sha256 {digest})")
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec-sha", required=True)
    parser.add_argument("--spec-repository", default=REPOSITORY)
    parser.add_argument("--contract-version", default="1")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    changed = update(root, args.spec_sha, args.spec_repository, args.contract_version)
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"changed={str(changed).lower()}\n")
            source = json.loads((root / "crates/braintrust-api-client/openapi-source.json").read_text())
            output.write(f"spec_sha={source['commit']}\n")


if __name__ == "__main__":
    main()
