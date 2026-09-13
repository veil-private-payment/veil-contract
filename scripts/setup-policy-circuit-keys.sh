#!/usr/bin/env bash
# Groth16 trusted setup for the policy transaction circuit.
#
# Produces the proving key and the verifying key for the compiled circuit.
# The setup is single-party: one contribution, generated locally. That is
# adequate for testnet and is stated as a limitation in the README.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CIRCUIT="policy_tx_2_2"
ARTIFACT_DIR="${POLICY_CIRCUIT_OUT_DIR:-$ROOT_DIR/target/circuits-artifacts/manual}"
KEY_DIR="${POLICY_CIRCUIT_KEY_DIR:-$ROOT_DIR/target/circuit-keys}"
PTAU_DIR="${PTAU_DIR:-$ROOT_DIR/target/ptau}"

# snarkjs sizes the setup by the total constraint count, linear ones included.
# The depth-16 circuit has 34894, so 2^16 = 65536 is the smallest that fits.
PTAU_POWER="${PTAU_POWER:-16}"
# Phase 1 comes from scripts/generate-powers-of-tau.sh. Point PTAU_FILE at a
# published multi-party transcript instead once one is reachable again.
PTAU_FILE="${PTAU_FILE:-$PTAU_DIR/veil_pot_${PTAU_POWER}.ptau}"

SNARKJS="$ROOT_DIR/node_modules/.bin/snarkjs"
R1CS="$ARTIFACT_DIR/$CIRCUIT.r1cs"

if [[ ! -x "$SNARKJS" ]]; then
  echo "Missing snarkjs. Run: pnpm install" >&2
  exit 1
fi

if [[ ! -f "$R1CS" ]]; then
  echo "Missing $R1CS. Run: make compile-policy-circuit" >&2
  exit 1
fi

mkdir -p "$PTAU_DIR" "$KEY_DIR"

if [[ ! -f "$PTAU_FILE" ]]; then
  echo "Missing $PTAU_FILE. Run: make generate-powers-of-tau" >&2
  exit 1
fi

echo "Verifying the Powers of Tau transcript"
"$SNARKJS" powersoftau verify "$PTAU_FILE"

echo "Running Groth16 setup"
"$SNARKJS" groth16 setup "$R1CS" "$PTAU_FILE" "$KEY_DIR/${CIRCUIT}_0000.zkey"

echo "Applying the contribution"
"$SNARKJS" zkey contribute \
  "$KEY_DIR/${CIRCUIT}_0000.zkey" \
  "$KEY_DIR/${CIRCUIT}_proving_key.zkey" \
  --name="veil-testnet-setup" \
  -e="${SETUP_ENTROPY:-$(head -c 64 /dev/urandom | od -An -tx1 | tr -d ' \n')}"

rm -f "$KEY_DIR/${CIRCUIT}_0000.zkey"

echo "Exporting the verifying key"
"$SNARKJS" zkey export verificationkey \
  "$KEY_DIR/${CIRCUIT}_proving_key.zkey" \
  "$KEY_DIR/${CIRCUIT}_vk.json"

echo "Verifying the proving key against the r1cs and the ptau"
"$SNARKJS" zkey verify "$R1CS" "$PTAU_FILE" "$KEY_DIR/${CIRCUIT}_proving_key.zkey"

echo "Keys written:"
printf '  %s\n' "$KEY_DIR/${CIRCUIT}_proving_key.zkey" "$KEY_DIR/${CIRCUIT}_vk.json"
