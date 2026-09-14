#!/usr/bin/env bash
# Check a phase 2 chain: every contribution, in order, against the circuit.
#
# This is the part a stranger can run. It proves the proving key really was
# built from this circuit and these Powers of Tau, and lists the contributions
# so each participant can find their own hash in the transcript.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SNARKJS="${SNARKJS:-npx --yes snarkjs@0.7.6}"
CIRCUIT="${CIRCUIT:-policy_tx_2_2}"
R1CS="${R1CS:-$ROOT_DIR/target/circuits/${CIRCUIT}.r1cs}"
PTAU_FILE="${PTAU_FILE:-$ROOT_DIR/target/ptau/veil_pot_16.ptau}"
ZKEY="${1:-$ROOT_DIR/circuit-keys/${CIRCUIT}_proving_key.zkey}"

for f in "$R1CS" "$PTAU_FILE" "$ZKEY"; do
  [[ -f "$f" ]] || { echo "Missing $f" >&2; exit 1; }
done

echo "Powers of Tau transcript"
$SNARKJS powersoftau verify "$PTAU_FILE"

echo
echo "Phase 2 chain for $(basename "$ZKEY")"
$SNARKJS zkey verify "$R1CS" "$PTAU_FILE" "$ZKEY"

# `zkey verify` prints every contribution's hash as it walks the chain, so
# there is nothing further to list.

echo
echo "sha256 $(shasum -a 256 "$ZKEY" | cut -d' ' -f1)"
echo "Compare that against the last line of circuit-keys/ceremony-transcript.md."
