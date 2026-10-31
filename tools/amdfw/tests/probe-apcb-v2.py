#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Investigate v2 container fidelity without changing the production dependency.

Usage: probe-apcb-v2.py AMD_APCB_CHECKOUT APCB_BIN... [--offline]
Use amd-apcb commit 1ec66d607de2437813cdd9ffa91719a4b163b3cf (0.5.5).
Inputs are extracted standalone APCBs, not complete vendor ROMs. The report is
JSON Lines. Byte-identical with opaque_records != [] is NOT fully typed support.
"""
import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("apcbs", type=Path, nargs="+")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    source = args.library.resolve(strict=True)
    inputs = [path.resolve(strict=True) for path in args.apcbs]
    fixtures = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix="fstart-apcb-v2-probe-") as directory:
        root = Path(directory)
        library = root / "library"
        library.mkdir()
        shutil.copytree(source / "src", library / "src")
        shutil.copy2(source / "LICENSE.txt", library / "LICENSE.txt")
        # Do not build the upstream checkout's maintenance-tool workspace.
        manifest = re.sub(r'^workspace\s*=\s*\{[^\n]*\}\s*\n', "",
                          (source / "Cargo.toml").read_text(), count=1,
                          flags=re.MULTILINE)
        (library / "Cargo.toml").write_text(manifest)
        subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-d", str(library),
                        "-i", str(fixtures / "v2-probe-library.patch")],
                       check=True, stdout=sys.stderr)
        # Separate workspace: fstart's host profiles use panic=abort. The probe
        # must be able to report upstream typed-accessor panics per input.
        (root / "src").mkdir()
        shutil.copy2(fixtures / "v2-probe.rs.in", root / "src/main.rs")
        (root / "Cargo.toml").write_text(
            '[package]\nname = "fstart-apcb-v2-probe"\nversion = "0.1.0"\n'
            'edition = "2024"\n[dependencies]\n'
            f'amd-apcb = {{ path = {json.dumps(str(library))}, features = ["serde"] }}\n'
            'serde_json = "1"\n[profile.dev]\npanic = "unwind"\n')
        command = ["cargo", "run", "--quiet", "--manifest-path", str(root / "Cargo.toml")]
        if args.offline:
            command.append("--offline")
        result = subprocess.run(command + ["--", *map(str, inputs)], cwd=root)
        return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
