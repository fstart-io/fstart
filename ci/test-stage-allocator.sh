#!/usr/bin/env bash
# Test the actual stage allocator without board linker symbols or payload pins.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/fstart-stage-allocator-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
cd "$ROOT"
python3 - "$ROOT" "$WORK" <<'PY'
import json
import pathlib
import sys
import tomllib

root, work = map(pathlib.Path, sys.argv[1:])
workspace = tomllib.loads((root / "Cargo.toml").read_text())
dep = workspace["workspace"]["dependencies"]["linked_list_allocator"]
lock = tomllib.loads((root / "Cargo.lock").read_text())
version = next(p["version"] for p in lock["package"] if p["name"] == "linked_list_allocator")
# Match the production dependency/features, including its exact locked version.
manifest = f'''[package]
name = "fstart-stage-allocator-regression"
version = "0.0.0"
edition = {json.dumps(workspace["workspace"]["package"]["edition"])}
[workspace]
[lib]
path = {json.dumps(str(root / "crates/stage/src/alloc.rs"))}
[features]
default = ["reclaiming-alloc"]
reclaiming-alloc = []
[dependencies]
linked_list_allocator = {{ version = "={version}", default-features = {str(dep.get("default-features", True)).lower()}, features = {json.dumps(dep.get("features", []))} }}
'''
(work / "Cargo.toml").write_text(manifest)
PY
cargo test --manifest-path "$WORK/Cargo.toml" --lib "$@" -- --test-threads=1
# Also compile/run the unchanged bump profile with the same test backing store.
cargo test --manifest-path "$WORK/Cargo.toml" --no-default-features --lib
cargo clippy --manifest-path "$WORK/Cargo.toml" --all-targets -- -D warnings
