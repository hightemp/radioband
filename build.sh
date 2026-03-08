#!/usr/bin/env bash
# Radioband — full build script
# Produces the docs/ directory for GitHub Pages deployment.
set -euo pipefail

export RUSTFLAGS='--cfg=web_sys_unstable_apis'

echo "==> Step 1: Build sdr-worker (WASM)"
cargo build --target wasm32-unknown-unknown --release -p sdr-worker

echo "==> Step 2: Generate worker JS bindings"
mkdir -p static/worker-pkg
wasm-bindgen \
    target/wasm32-unknown-unknown/release/sdr_worker.wasm \
    --out-dir static/worker-pkg \
    --target no-modules \
    --no-typescript \
    --omit-default-module-path

echo "==> Step 3: Build main app with Trunk"
trunk build --release

echo "==> Done.  Output is in docs/"
ls -lh docs/
