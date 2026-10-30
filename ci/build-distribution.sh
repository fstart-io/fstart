#!/usr/bin/env bash
# Build all redistributable outputs. No donor firmware is fetched by CI.
set -euo pipefail
cd "$(dirname "$0")/.."
export SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
revision="$(git rev-parse HEAD)"
out=target/distribution
rm -rf "$out" target/image-web
mkdir -p "$out" target/image-web
cargo build --locked --release -p fbuild -p fstart-amdfw-tools
# fbuild discovers every base board AND named variant, then asks its platform
# for supported payload selections. Unsupported combinations are not guessed.
target/release/fbuild bundle --out "$out" --revision "$revision"
cargo build --locked --release --target wasm32-unknown-unknown -p fstart-image-web
wasm-bindgen --target web --out-dir target/image-web/pkg target/wasm32-unknown-unknown/release/fstart_image_web.wasm
wasm-bindgen --target nodejs --out-dir target/image-web/node target/wasm32-unknown-unknown/release/fstart_image_web.wasm
cp tools/image-web/client.mjs target/image-web/
printf '{"format_version":1,"bundle_format_version":2,"revision":"%s","bindings":"pkg/fstart_image_web.js","client":"client.mjs"}\n' "$revision" > target/image-web/sdk.json
touch target/image-web/.nojekyll
node --test tools/image-web/proxy/worker.test.mjs tools/image-web/client.test.mjs
python3 -m unittest discover -s ci -p 'test_*.py'
node ci/test-web.mjs "$out" target/image-web/node/fstart_image_web.js target/release/fbuild
# Keep Pages frontend-only; release archives additionally carry versioned SDKs.
python3 - <<'PY'
from pathlib import Path
from zipfile import ZipFile, ZIP_DEFLATED
root = Path('target/image-web')
with ZipFile('target/distribution/image-web.zip', 'w', compression=ZIP_DEFLATED, compresslevel=9) as archive:
    for path in sorted(root.rglob('*')):
        if path.is_file() and 'node' not in path.relative_to(root).parts:
            archive.write(path, path.relative_to(root))
PY
tar -C target/release -czf "$out/host-tools-linux-x86_64.tar.gz" fbuild fstart-amdfw fstart-apcb
# Node bindings are test-only, not part of the Pages deployment.
rm -rf target/image-web/node
