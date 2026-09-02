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

# 2^15 = 32768 constraints, above the circuit's current constraint count.
PTAU_POWER="${PTAU_POWER:-15}"
PTAU_FILE="$PTAU_DIR/powersOfTau28_hez_final_${PTAU_POWER}.ptau"
PTAU_URL="https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_${PTAU_POWER}.ptau"

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
  echo "Downloading Powers of Tau 2^$PTAU_POWER"
  curl -sfL --retry 3 "$PTAU_URL" -o "$PTAU_FILE.tmp"
  mv "$PTAU_FILE.tmp" "$PTAU_FILE"
fi

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
