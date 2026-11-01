#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
RUST_VERSION="1.99.1"
WORKER_BUILD_VERSION="0.8.7"

if ! command -v rustup >/dev/null 2>&1; then
  echo "==> Installing pinned Rust toolchain..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION"
fi
rustup toolchain install "$RUST_VERSION" --profile minimal --target wasm32-unknown-unknown
rustup target add --toolchain "$RUST_VERSION" wasm32-unknown-unknown
export RUSTUP_TOOLCHAIN="$RUST_VERSION"

if ! command -v worker-build >/dev/null 2>&1 \
    || [[ "$(worker-build --version)" != "worker-build $WORKER_BUILD_VERSION" ]]; then
  echo "==> Installing worker-build $WORKER_BUILD_VERSION..."
  cargo install worker-build --version "=$WORKER_BUILD_VERSION" --locked --force
fi

# Resolve nothing implicitly during the production build.
cargo metadata --locked --format-version 1 >/dev/null
export CARGO_NET_OFFLINE=true
echo "==> Building worker WASM..."
worker-build --release --locked