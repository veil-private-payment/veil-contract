#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_LOCAL_CIRCOM="$ROOT_DIR/target/cargo-tools/bin/circom"
CIRCOM_BIN="${CIRCOM_BIN:-}"
EXPECTED_VERSION="${CIRCOM_EXPECTED_VERSION:-circom compiler 2.2.3}"
OUT_DIR="${POLICY_CIRCUIT_OUT_DIR:-$ROOT_DIR/target/circuits-artifacts/manual}"
ENTRYPOINT="$ROOT_DIR/circuits/src/policy_tx_2_2.circom"

if [[ -z "$CIRCOM_BIN" ]]; then
  if [[ -x "$DEFAULT_LOCAL_CIRCOM" ]]; then
    CIRCOM_BIN="$DEFAULT_LOCAL_CIRCOM"
  elif command -v circom >/dev/null 2>&1; then
    CIRCOM_BIN="$(command -v circom)"
  else
    echo "Missing circom compiler. Run: make install-circom" >&2
    exit 1
  fi
fi

actual_version="$("$CIRCOM_BIN" --version)"
if [[ "$actual_version" != "$EXPECTED_VERSION" ]]; then
  echo "Unexpected circom version: $actual_version" >&2
  echo "Expected: $EXPECTED_VERSION" >&2
  echo "Set CIRCOM_BIN only when intentionally overriding the compiler." >&2
  exit 1
fi

"$ROOT_DIR/scripts/fetch-circomlib.sh"
mkdir -p "$OUT_DIR"

"$CIRCOM_BIN" "$ENTRYPOINT" --r1cs --wasm --sym -o "$OUT_DIR"

required_outputs=(
  "$OUT_DIR/policy_tx_2_2.r1cs"
  "$OUT_DIR/policy_tx_2_2.sym"
  "$OUT_DIR/policy_tx_2_2_js/policy_tx_2_2.wasm"
)

for artifact in "${required_outputs[@]}"; do
  if [[ ! -f "$artifact" ]]; then
    echo "Missing expected compiled artifact: $artifact" >&2
    exit 1
  fi
done

echo "Policy circuit compiled:"
printf '  %s\n' "${required_outputs[@]}"
