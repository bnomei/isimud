#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?TARGET is required}"
BIN_NAME="${BIN_NAME:-isimud}"

cargo build --release --bin "$BIN_NAME" --target "$TARGET"
