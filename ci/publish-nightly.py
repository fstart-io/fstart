#!/usr/bin/env python3
"""Promote a successful trusted CI artifact; never execute downloaded files.
GitHub Releases have no native expiration, so prune only releases owned here.
"""
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

UTC = dt.UTC
MARKER = "fstart-nightly-v1"
NIGHTLY = re.compile(r"nightly-\d{4}-\d{2}-\d{2}-[0-9a-f]{12}\Z")
DAYS = 30
FORMAT_VERSION = 2


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True).strip()


def api(repo, path):
    return json.loads(gh("api", f"repos/{repo}/{path}"))


def owned(release):
    return bool(NIGHTLY.fullmatch(release["tag_name"])) and MARKER in (release.get("body") or "")


def expiration(release):
    match = re.search(r"expires_at=(\S+)", release.get("body") or "")
    if not match:
        return None
    try:
        return dt.datetime.fromisoformat(match[1].replace("Z", "+00:00"))
    except ValueError:
        return None


def prune_candidates(releases, now):
    # Explicitly promoting a nightly to a stable release opts out of expiry.
    managed = [r for r in releases if owned(r) and (r.get("prerelease", False) or r["draft"])]
    published = [r for r in managed if not r["draft"]]
    newest = max(published, key=lambda r: r["published_at"], default=None)
    return [r for r in managed if r is not newest and expiration(r) is not None and expiration(r) < now]


def verify(directory, revision):
    catalog = json.loads((directory / "catalog.json").read_text())
    if catalog["format_version"] != FORMAT_VERSION or catalog["revision"] != revision or not catalog["builds"]:
        raise ValueError("artifact catalog does not match successful CI revision")
    for build in catalog["builds"]:
        name = build["asset"]
        if Path(name).name != name or not name.endswith(".bundle.zip"):
            raise ValueError("invalid bundle asset name")
        data = (directory / name).read_bytes()
        if len(data) != build["size"] or hashlib.sha256(data).hexdigest() != build["sha256"]:
            raise ValueError(f"bundle identity mismatch: {name}")
    expected = {b["asset"] for b in catalog["builds"]} | {"catalog.json", "image-web.zip", "host-tools-linux-x86_64.tar.gz"}
    if {p.name for p in directory.iterdir()} != expected:
        raise ValueError("unexpected or missing distribution assets")
    if len(expected) + 2 > 1000:
        raise ValueError("release exceeds GitHub's 1000-asset limit")
    return catalog


def main():
    repo = os.environ.get("GITHUB_REPOSITORY", "fstart-io/fstart")
    # Follow all release pages so stable releases never hide older nightlies.
    pages = json.loads(gh("api", "--paginate", "--slurp", f"repos/{repo}/releases?per_page=100"))
    releases = [release for page in pages for release in page]
    runs = api(repo, "actions/workflows/ci.yml/runs?branch=master&event=push&status=success&per_page=100")["workflow_runs"]
    runs = [r for r in runs if r["head_branch"] == "master" and r["event"] == "push" and r["conclusion"] == "success"]
    if not runs:
        raise RuntimeError("no successful master CI run is available")
    run = runs[0]
    revision = run["head_sha"]
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("invalid CI revision")
    now = dt.datetime.now(UTC)
    def stamp(value):
        return value.isoformat(timespec="seconds").replace("+00:00", "Z")
    # No rebuild/re-release when master has not changed. Keep the last good
    # nightly even after its advertised expiry until a replacement exists.
    if not any(owned(r) and not r["draft"] and r["target_commitish"] == revision for r in releases):
        artifacts = api(repo, f"actions/runs/{run['id']}/artifacts")["artifacts"]
        if not any(a["name"] == "distribution" and not a["expired"] for a in artifacts):
            raise RuntimeError("successful CI has no unexpired distribution artifact")
        tag = f"nightly-{now:%Y-%m-%d}-{revision[:12]}"
        if any(r["tag_name"] == tag for r in releases):
            raise RuntimeError(f"{tag} already exists; inspect/remove the incomplete draft before retrying")
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            gh("run", "download", str(run["id"]), "--repo", repo, "--name", "distribution", "--dir", temp)
            catalog = verify(directory, revision)
            metadata = {"format_version": FORMAT_VERSION, "revision": revision, "created_at": stamp(now),
                        "expires_at": stamp(now + dt.timedelta(days=DAYS)), "source_run_id": run["id"]}
            (directory / "release.json").write_text(json.dumps(metadata, indent=2) + "\n")
            catalog.update(metadata)
            (directory / "catalog.json").write_text(json.dumps(catalog, indent=2) + "\n")
            sums = "".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n" for path in sorted(directory.iterdir()))
            (directory / "SHA256SUMS").write_text(sums)
            notes = (f"{MARKER}\nrevision={revision}\nexpires_at={metadata['expires_at']}\n\n"
                     "All declared boards, variants, payload selections and unit binaries. "
                     "Vendor firmware is not included. See catalog.json for required local inputs. "
                     "Nightlies expire after 30 days; the latest successful nightly is retained. "
                     "Locally assembled images use an ephemeral signing key unless you provide your own.\n")
            gh("release", "create", tag, "--repo", repo, "--target", revision, "--title", f"Nightly {now:%Y-%m-%d}",
               "--notes", notes, "--prerelease", "--latest=false", "--draft", *map(str, sorted(directory.iterdir())))
            gh("release", "edit", tag, "--repo", repo, "--draft=false")
            releases.append({"tag_name": tag, "draft": False, "prerelease": True, "published_at": stamp(now), "body": notes})
    for release in prune_candidates(releases, now):
        gh("release", "delete", release["tag_name"], "--repo", repo, "--cleanup-tag", "--yes")


if __name__ == "__main__":
    main()
