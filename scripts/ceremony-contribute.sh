#!/usr/bin/env bash
# One participant's turn in the phase 2 ceremony.
#
# The proving key is only as trustworthy as the assumption that at least one
# contributor destroyed their randomness. One contributor means one person to
# trust, or to compromise; that is what this replaces.
#
# Usage:  scripts/ceremony-contribute.sh <input.zkey> <output.zkey> "<your name>"
#
# Take the input file from the previous participant, run this, publish the
# hash it prints, and send only the output file onward. Never send the entropy
# anywhere, and do not keep it: security comes from it being gone.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SNARKJS="${SNARKJS:-npx --yes snarkjs@0.7.6}"

IN_ZKEY="${1:?usage: ceremony-contribute.sh <input.zkey> <output.zkey> <name>}"
OUT_ZKEY="${2:?usage: ceremony-contribute.sh <input.zkey> <output.zkey> <name>}"
NAME="${3:?usage: ceremony-contribute.sh <input.zkey> <output.zkey> <name>}"
TRANSCRIPT="${CEREMONY_TRANSCRIPT:-$ROOT_DIR/circuit-keys/ceremony-transcript.md}"

if [[ ! -f "$IN_ZKEY" ]]; then
  echo "No such file: $IN_ZKEY" >&2
  exit 1
fi

# Entropy the participant supplies, or fresh randomness from the machine. Read
# from the environment so it never reaches the shell history or the process
# list of another user.
ENTROPY="${CEREMONY_ENTROPY:-$(head -c 64 /dev/urandom | od -An -tx1 | tr -d ' \n')}"

echo "Contributing as '$NAME'"
$SNARKJS zkey contribute "$IN_ZKEY" "$OUT_ZKEY" --name="$NAME" -e="$ENTROPY"
unset ENTROPY

HASH="$(shasum -a 256 "$OUT_ZKEY" | cut -d' ' -f1)"

mkdir -p "$(dirname "$TRANSCRIPT")"
if [[ ! -f "$TRANSCRIPT" ]]; then
  cat > "$TRANSCRIPT" <<'HEADER'
# Phase 2 ceremony transcript

Each line is one contribution. Anyone can check the chain with
`scripts/ceremony-verify.sh`, and each participant should confirm their own
line matches the hash they saw.

| # | Contributor | SHA-256 of the resulting zkey |
|---|---|---|
HEADER
fi

COUNT=$(( $(grep -c '^| [0-9]' "$TRANSCRIPT" || true) + 1 ))
printf '| %s | %s | `%s` |\n' "$COUNT" "$NAME" "$HASH" >> "$TRANSCRIPT"

cat <<INFO

Contribution $COUNT recorded.

  file   $OUT_ZKEY
  sha256 $HASH

Publish that hash somewhere public, under your own name. Send only the file to
the next participant. Your entropy is gone the moment this script exits, and it
should be: the setup is sound as long as one participant's randomness is
unrecoverable.
INFO
