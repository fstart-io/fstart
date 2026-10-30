import datetime as dt
import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("nightly", Path(__file__).with_name("publish-nightly.py"))
assert spec is not None and spec.loader is not None
nightly = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nightly)


def release(day, *, stable=False, draft=False):
    date = dt.datetime(2026, 1, day, tzinfo=dt.UTC)
    return {"tag_name": "v1.0" if stable else f"nightly-2026-01-{day:02}-abcdef123456",
            "draft": draft, "prerelease": not stable, "published_at": date.isoformat(),
            "body": f"{nightly.MARKER}\nexpires_at={(date + dt.timedelta(days=30)).isoformat()}"}


class RetentionTests(unittest.TestCase):
    def test_never_deletes_stable_or_latest_even_when_expired(self):
        old, latest, stable = release(1), release(2), release(3, stable=True)
        self.assertEqual(nightly.prune_candidates([old, latest, stable], dt.datetime(2026, 4, 1, tzinfo=dt.UTC)), [old])

    def test_promoted_nightly_is_retained_as_stable(self):
        promoted, newer = release(1), release(2)
        promoted["prerelease"] = False
        self.assertEqual(nightly.prune_candidates([promoted, newer], dt.datetime(2026, 4, 1, tzinfo=dt.UTC)), [])

    def test_unexpired_and_unowned_are_kept(self):
        old, latest = release(1), release(3)
        unowned = release(2)
        unowned["body"] = "manual release"
        self.assertEqual(nightly.prune_candidates([old, latest, unowned], dt.datetime(2026, 1, 20, tzinfo=dt.UTC)), [])

    def test_expired_drafts_do_not_protect_older_releases(self):
        old, latest, draft = release(1), release(2), release(3, draft=True)
        self.assertEqual(nightly.prune_candidates([old, latest, draft], dt.datetime(2026, 4, 1, tzinfo=dt.UTC)), [old, draft])

    def test_verifies_catalog_and_exact_asset_set(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            data = b"compressed build"
            (root / "board.bundle.zip").write_bytes(data)
            catalog = {"format_version": nightly.FORMAT_VERSION, "revision": "abc", "builds": [{"asset": "board.bundle.zip", "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}]}
            (root / "catalog.json").write_text(json.dumps(catalog))
            (root / "image-web.zip").write_bytes(b"web")
            (root / "host-tools-linux-x86_64.tar.gz").write_bytes(b"tools")
            nightly.verify(root, "abc")
            catalog["format_version"] = 1
            (root / "catalog.json").write_text(json.dumps(catalog))
            with self.assertRaises(ValueError):
                nightly.verify(root, "abc")
            catalog["format_version"] = nightly.FORMAT_VERSION
            (root / "catalog.json").write_text(json.dumps(catalog))
            with self.assertRaises(ValueError):
                nightly.verify(root, "other")
            (root / "vendor.rom").write_bytes(b"do not publish")
            with self.assertRaises(ValueError):
                nightly.verify(root, "abc")
            (root / "vendor.rom").unlink()
            (root / "board.bundle.zip").write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                nightly.verify(root, "abc")


class PublicationTests(unittest.TestCase):
    def test_promotes_verified_artifacts_as_a_draft_before_publishing(self):
        revision = "a" * 40
        calls = []
        published_catalog = {}

        def mock_api(_repo, path):
            if path.startswith("actions/workflows/"):
                return {"workflow_runs": [{"id": 42, "head_sha": revision, "head_branch": "master", "event": "push", "conclusion": "success"}]}
            return {"artifacts": [{"name": "distribution", "expired": False}]}

        def mock_gh(*args):
            calls.append(args)
            if args[0] == "api":
                return "[[]]"
            if args[:2] == ("run", "download"):
                root = Path(args[args.index("--dir") + 1])
                data = b"bundle"
                (root / "board.bundle.zip").write_bytes(data)
                (root / "catalog.json").write_text(json.dumps({"format_version": nightly.FORMAT_VERSION, "revision": revision, "builds": [{"asset": "board.bundle.zip", "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}]}))
                for name in ["image-web.zip", "host-tools-linux-x86_64.tar.gz"]:
                    (root / name).write_bytes(b"asset")
            if args[:2] == ("release", "create"):
                self.assertIn("--draft", args)
                catalog_path = next(Path(arg) for arg in args if arg.endswith("/catalog.json"))
                published_catalog.update(json.loads(catalog_path.read_text()))
            return ""

        with patch.object(nightly, "gh", mock_gh), patch.object(nightly, "api", mock_api):
            nightly.main()
        create = next(i for i, args in enumerate(calls) if args[:2] == ("release", "create"))
        edit = next(i for i, args in enumerate(calls) if args[:2] == ("release", "edit"))
        self.assertLess(create, edit)
        self.assertIn("--draft=false", calls[edit])
        self.assertEqual(published_catalog["revision"], revision)
        self.assertEqual(published_catalog["source_run_id"], 42)
        self.assertIn("expires_at", published_catalog)


if __name__ == "__main__":
    unittest.main()
