import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

module_spec = importlib.util.spec_from_file_location("update_openapi", Path(__file__).with_name("update-openapi.py"))
updater = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(updater)
OLD = "a" * 40
NEW = "b" * 40
API = f"https://api.github.com/repos/{updater.REPOSITORY}"
DATA = json.dumps({"openapi": "3.0.3", "paths": {"/v1/dataset": {}}}).encode()


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "crates/braintrust-api-client"
        self.responses = {
            f"{API}/commits/main": {"sha": NEW},
            f"{API}/compare/{NEW}...{NEW}": {"status": "identical"},
            f"{API}/compare/{OLD}...{NEW}": {"status": "ahead"},
            f"https://raw.githubusercontent.com/{updater.REPOSITORY}/{NEW}/openapi/spec.json": DATA,
        }

    def fetch(self, url):
        value = self.responses[url]
        return value if isinstance(value, bytes) else json.dumps(value).encode()

    def write_previous(self, sha=OLD, data=b"previous"):
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / "openapi.json").write_bytes(data)
        (self.directory / "openapi-source.json").write_text(json.dumps({
            "repository": updater.REPOSITORY, "commit": sha,
            "sha256": hashlib.sha256(data).hexdigest(),
        }))

    def test_update_records_exact_bytes_and_source_digest(self):
        self.write_previous()
        self.assertTrue(updater.update(self.root, NEW, fetch=self.fetch))
        self.assertEqual((self.directory / "openapi.json").read_bytes(), DATA)
        source = json.loads((self.directory / "openapi-source.json").read_text())
        self.assertEqual(source["commit"], NEW)
        self.assertEqual(source["sha256"], hashlib.sha256(DATA).hexdigest())

    def test_unchanged_content_does_not_open_metadata_only_pr(self):
        self.write_previous(data=DATA)
        original = (self.directory / "openapi-source.json").read_bytes()
        self.assertFalse(updater.update(self.root, NEW, fetch=self.fetch))
        self.assertEqual((self.directory / "openapi-source.json").read_bytes(), original)

    def test_delayed_notification_coalesces_to_latest_published_commit(self):
        self.write_previous()
        self.assertTrue(updater.update(self.root, OLD, fetch=self.fetch))
        self.assertEqual(json.loads((self.directory / "openapi-source.json").read_text())["commit"], NEW)

    def test_unpublished_commit_is_rejected_without_changing_files(self):
        self.write_previous()
        self.responses[f"{API}/compare/{OLD}...{NEW}"] = {"status": "diverged"}
        with self.assertRaisesRegex(ValueError, "has not landed"):
            updater.update(self.root, OLD, fetch=self.fetch)
        self.assertEqual((self.directory / "openapi.json").read_bytes(), b"previous")

    def test_newer_snapshot_is_never_downgraded(self):
        self.write_previous(sha="c" * 40)
        self.responses[f"{API}/compare/{'c' * 40}...{NEW}"] = {"status": "behind"}
        self.assertFalse(updater.update(self.root, NEW, fetch=self.fetch))
        self.assertEqual((self.directory / "openapi.json").read_bytes(), b"previous")

    def test_invalid_inputs_are_rejected_before_fetching(self):
        def unexpected(_):
            self.fail("invalid inputs must not fetch")
        for sha, repository, version in [
            ("main", updater.REPOSITORY, "1"),
            (NEW, "someone/else", "1"),
            (NEW, updater.REPOSITORY, "2"),
        ]:
            with self.subTest(sha=sha, repository=repository, version=version):
                with self.assertRaises(ValueError):
                    updater.update(self.root, sha, repository, version, unexpected)

    def test_invalid_spec_does_not_replace_snapshot(self):
        self.write_previous()
        self.responses[f"https://raw.githubusercontent.com/{updater.REPOSITORY}/{NEW}/openapi/spec.json"] = b'{"openapi":"3.1.0","paths":{}}'
        with self.assertRaisesRegex(ValueError, "OpenAPI 3.0"):
            updater.update(self.root, NEW, fetch=self.fetch)
        self.assertEqual((self.directory / "openapi.json").read_bytes(), b"previous")


if __name__ == "__main__":
    unittest.main()
