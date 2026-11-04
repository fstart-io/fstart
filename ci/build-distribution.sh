#!/usr/bin/env bash
# Build all redistributable outputs. No donor firmware is fetched by CI.
set -euo pipefail
cd "$(dirname "$0")/.."
export SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
revision="$(git rev-parse HEAD)"
out=target/distribution
rm -rf "$out"
mkdir -p "$out"
cargo build --locked --release -p fbuild -p fstart-amdfw-tools
# The coreboot payload bundles ship SeaBIOS (fstart-romfiles branch), so
# those builds need no file from the user.
seabios=()
if [ -d target/seabios-src ]; then
  ci/payloads/seabios/build.sh target/seabios-src target/seabios >/dev/null
  seabios=(--seabios target/seabios)
fi
# fbuild discovers every base board AND named variant, then asks its platform
# for supported payload selections. Unsupported combinations are not guessed.
target/release/fbuild bundle --out "$out" --revision "$revision" "${seabios[@]}"
tar -C target/release -czf "$out/host-tools-linux-x86_64.tar.gz" fbuild fstart-amdfw fstart-apcb
