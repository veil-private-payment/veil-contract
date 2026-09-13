#!/usr/bin/env bash
# Generate the Powers of Tau (phase 1) for the policy circuit, locally.
#
# The canonical Hermez phase 1 transcript is normally downloaded and reused,
# because it carries contributions from many independent parties. That file is
# currently unreachable: every published mirror returns AccessDenied. This
# script produces a local substitute so the build is reproducible without it.
#
# The result is a SINGLE-PARTY phase 1. Anyone who kept the randomness used
# here could forge proofs. It is adequate for a testnet deployment and it is
# not adequate for holding value. Replace it with a multi-party ceremony before
# mainnet, and regenerate every key that derives from it.
#
# The final beacon uses the hash of a Stellar mainnet ledger, recorded in
# circuits/README.md so a reviewer can look it up. The ledger had already
# closed when it was chosen, so it documents the input rather than proving it
# was unpredictable.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PTAU_DIR="${PTAU_DIR:-$ROOT_DIR/target/ptau}"
POWER="${PTAU_POWER:-16}"
OUT="$PTAU_DIR/veil_pot_${POWER}.ptau"
SNARKJS="$ROOT_DIR/node_modules/.bin/snarkjs"

# Stellar mainnet ledger 64411842, closed 2026-09-13T16:36:31Z.
BEACON="${BEACON_HASH:-000423b3e4be70b451d0b04217c590b4a10cd90984d2278edcf1763dfbe6bbae}"
BEACON_ITERS="${BEACON_ITERS:-10}"

if [[ ! -x "$SNARKJS" ]]; then
  echo "Missing snarkjs. Run: pnpm install" >&2
  exit 1
fi

mkdir -p "$PTAU_DIR"

if [[ -f "$OUT" ]]; then
  echo "$OUT already exists; delete it to regenerate."
  exit 0
fi

echo "Starting the ceremony"
"$SNARKJS" powersoftau new bn128 "$POWER" "$PTAU_DIR/pot_0000.ptau"

echo "Contributing"
"$SNARKJS" powersoftau contribute \
  "$PTAU_DIR/pot_0000.ptau" "$PTAU_DIR/pot_0001.ptau" \
  --name="veil-local-phase1" \
  -e="$(head -c 64 /dev/urandom | od -An -tx1 | tr -d ' \n')"

echo "Applying the beacon"
"$SNARKJS" powersoftau beacon \
  "$PTAU_DIR/pot_0001.ptau" "$PTAU_DIR/pot_beacon.ptau" \
  "$BEACON" "$BEACON_ITERS" -n="stellar-ledger-64411842"

echo "Preparing phase 2"
"$SNARKJS" powersoftau prepare phase2 "$PTAU_DIR/pot_beacon.ptau" "$OUT"

echo "Verifying the transcript"
"$SNARKJS" powersoftau verify "$OUT"

rm -f "$PTAU_DIR/pot_0000.ptau" "$PTAU_DIR/pot_0001.ptau" "$PTAU_DIR/pot_beacon.ptau"
echo "Wrote $OUT"
