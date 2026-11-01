#!/bin/bash
set -e

# 如果没有 cargo，自动安装 rust 环境
if ! command -v cargo &> /dev/null; then
  echo "==> Cargo not found, installing Rust toolchain..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --target wasm32-unknown-unknown
  source "$HOME/.cargo/env"
fi

# 确保 wasm32 目标存在
rustup target add wasm32-unknown-unknown || true

# 安装兼容 worker 0.4 的 worker-build 版本
if ! command -v worker-build &> /dev/null; then
  echo "==> Installing worker-build (compatible with worker 0.4)..."
  cargo install worker-build --version "^0.1"
fi

# 编译 worker
echo "==> Building worker WASM..."
worker-build --release