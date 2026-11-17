#!/usr/bin/env bash
# Build all redistributable outputs. No donor firmware is fetched by CI.
set -euo pipefail
cd "$(dirname "$0")/.."
export SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
revision="$(git rev-parse HEAD)"
out=target/distribution
rm -rf "$out" target/image-web
mkdir -p "$out" target/image-web
cargo build --locked --release -p fbuild -p fstart-amdfw-tools -p fstart-release
# The coreboot payload bundles ship SeaBIOS (fstart-romfiles branch), so
# those builds need no file from the user.
seabios=()
if [ -d target/seabios-src ]; then
  ci/payloads/seabios/build.sh target/seabios-src target/seabios >/dev/null
  seabios=(--seabios target/seabios)
fi
# fbuild discovers every base board and named variant, and publishes the
# payloads its platform's plan takes.
target/release/fbuild bundle --out "$out" --revision "$revision" "${seabios[@]}"
cargo build --locked --release --target wasm32-unknown-unknown -p fstart-image-web
wasm-bindgen --target web --out-dir target/image-web/pkg target/wasm32-unknown-unknown/release/fstart_image_web.wasm
wasm-bindgen --target nodejs --out-dir target/image-web/node target/wasm32-unknown-unknown/release/fstart_image_web.wasm
# The web UI is static: its modules, the client helper and the bindings.
cp tools/image-web/www/* tools/image-web/client.mjs target/image-web/
printf '{"format_version":1,"bundle_format_version":3,"revision":"%s","bindings":"pkg/fstart_image_web.js","client":"client.mjs"}\n' "$revision" > target/image-web/sdk.json
touch target/image-web/.nojekyll
node --test tools/image-web/proxy/worker.test.mjs tools/image-web/client.test.mjs
cargo test --locked -p fstart-release
node ci/test-web.mjs "$out" target/image-web/node/fstart_image_web.js target/release/fbuild
# Keep Pages frontend-only; release archives additionally carry versioned SDKs.
target/release/fstart-release pack-sdk --root target/image-web --out "$out/image-web.zip"
tar -C target/release -czf "$out/host-tools-linux-x86_64.tar.gz" fbuild fstart-amdfw
# Node bindings are test-only, not part of the Pages deployment.
rm -rf target/image-web/node
