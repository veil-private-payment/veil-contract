#!/usr/bin/env bash
# Check that what is deployed is what this source builds, and show exactly what
# a shielded payment puts on the public ledger.
#
# Two questions a stranger should be able to answer without trusting anybody:
#
#   1. Is the code running on chain the code in this repository?
#   2. What does that code actually disclose?
#
# The first is a hash comparison. The second is reading a real transaction.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NETWORK="${NETWORK:-testnet}"
MANIFEST="${MANIFEST:-$ROOT_DIR/deployments/${NETWORK}.json}"
HORIZON="${HORIZON:-https://horizon-testnet.stellar.org}"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

POOL=$(jq -r '.contracts.pool' "$MANIFEST")
VERIFIER=$(jq -r '.contracts.verifier' "$MANIFEST")
ASP=$(jq -r '.contracts.aspMembership' "$MANIFEST")

echo "== 1. Deployed code against this source =="
echo "Building locally (release, wasm32v1-none)"
(cd "$ROOT_DIR" && stellar contract build >/dev/null)

check() {
  local name="$1" id="$2" wasm="$3"
  local onchain local_hash
  onchain=$(stellar contract fetch --id "$id" --network "$NETWORK" 2>/dev/null | shasum -a 256 | cut -d' ' -f1)
  local_hash=$(shasum -a 256 "$ROOT_DIR/target/wasm32v1-none/release/$wasm" | cut -d' ' -f1)
  if [[ "$onchain" == "$local_hash" ]]; then
    printf '  %-10s match    %s\n' "$name" "$onchain"
  else
    printf '  %-10s DIFFERS\n    on chain %s\n    local    %s\n' "$name" "$onchain" "$local_hash"
  fi
}

check pool "$POOL" pool.wasm
check verifier "$VERIFIER" circom_groth16_verifier.wasm
check asp "$ASP" asp_membership.wasm

echo
echo "A matching verifier is the part that carries weight: its verifying key is"
echo "compiled in, so an identical hash means the key checking proofs on chain is"
echo "the one this repository's ceremony produced."

echo
echo "== 2. What a shielded payment discloses =="
TX="${1:-$(jq -r '.transactions.spendAccepted' "$MANIFEST")}"
echo "Transaction $TX"

curl -sS "$HORIZON/transactions/$TX" | jq -r '.envelope_xdr' > "$ROOT_DIR/target/tx.xdr"
stellar xdr decode --type TransactionEnvelope --input single-base64 --output json \
  < "$ROOT_DIR/target/tx.xdr" > "$ROOT_DIR/target/tx.json"

python3 - "$ROOT_DIR/target/tx.json" <<'PY'
import json, sys

tx = json.load(open(sys.argv[1]))
op = tx["tx"]["tx"]["operations"][0]["body"]["invoke_host_function"]["host_function"]["invoke_contract"]
print(f"  function      {op['function_name']}")
print(f"  submitted by  {op['args'][2]['address']}   (public: pays the fee)")
print("  ExtData, the part that is not a proof:")
for entry in op["args"][1]["map"]:
    key = entry["key"]["symbol"]
    val = entry["val"]
    kind = next(iter(val))
    if kind == "bytes":
        print(f"    {key:18} {len(val['bytes']) // 2} bytes of ciphertext")
    elif kind == "address":
        print(f"    {key:18} {val['address']}")
    else:
        print(f"    {key:18} {val[kind]}")
PY

cat <<'NOTE'

  Read it as: ext_amount 0 means the ledger carries no figure for a shielded
  send, and `recipient` is unused for one — it holds the sender's own account,
  which is public anyway as the transaction's source. Both outputs carry
  ciphertext of the same length, so nothing is learned from their shape.

  What a shielded payment does disclose: that this account made one, when, and
  the fee it paid. Privacy comes from not being able to tell which deposit
  funded it or where it went, and that holds only as far as the pool is large
  enough to hide in.
NOTE
