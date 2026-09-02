#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL_ROOT="${CIRCOM_TOOL_ROOT:-$ROOT_DIR/target/cargo-tools}"
CIRCOM_BIN="$TOOL_ROOT/bin/circom"
CIRCOM_TAG="${CIRCOM_TAG:-v2.2.3}"
EXPECTED_VERSION="${CIRCOM_EXPECTED_VERSION:-circom compiler 2.2.3}"

if [[ -x "$CIRCOM_BIN" ]]; then
  current_version="$("$CIRCOM_BIN" --version)"
  if [[ "$current_version" == "$EXPECTED_VERSION" ]]; then
    echo "$current_version already installed at $CIRCOM_BIN"
    exit 0
  fi

  echo "Found $current_version at $CIRCOM_BIN; reinstalling $EXPECTED_VERSION."
fi

cargo install \
  --git https://github.com/iden3/circom.git \
  --tag "$CIRCOM_TAG" \
  circom \
  --root "$TOOL_ROOT" \
  --locked

"$CIRCOM_BIN" --version
