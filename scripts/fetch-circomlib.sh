#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCK_FILE="$ROOT_DIR/circuits/circomlib.lock"
DEST_DIR="$ROOT_DIR/circuits/src/circomlib"
REMOTE_URL="${CIRCOMLIB_REMOTE_URL:-https://github.com/iden3/circomlib.git}"
FORCE="${CIRCOMLIB_FORCE:-0}"

if [[ ! -f "$LOCK_FILE" ]]; then
  echo "Missing circomlib lock file: $LOCK_FILE" >&2
  exit 1
fi

REVISION="$(tr -d '[:space:]' < "$LOCK_FILE")"
if [[ ! "$REVISION" =~ ^[0-9a-fA-F]{40}$ ]]; then
  echo "Invalid circomlib revision in $LOCK_FILE: $REVISION" >&2
  exit 1
fi

if [[ -e "$DEST_DIR" && ! -d "$DEST_DIR/.git" ]]; then
  if [[ "$FORCE" == "1" ]]; then
    rm -rf "$DEST_DIR"
  else
    echo "$DEST_DIR exists but is not a git checkout." >&2
    echo "Remove it manually or rerun with CIRCOMLIB_FORCE=1." >&2
    exit 1
  fi
fi

if [[ ! -d "$DEST_DIR/.git" ]]; then
  mkdir -p "$DEST_DIR"
  git -C "$DEST_DIR" init --quiet
  git -C "$DEST_DIR" remote add origin "$REMOTE_URL"
fi

current_head="$(git -C "$DEST_DIR" rev-parse HEAD 2>/dev/null || true)"
if [[ "$current_head" == "$REVISION" ]]; then
  echo "circomlib already at $REVISION"
  exit 0
fi

git -C "$DEST_DIR" fetch --depth 1 origin "$REVISION"
git -C "$DEST_DIR" checkout --detach FETCH_HEAD --quiet

resolved_head="$(git -C "$DEST_DIR" rev-parse HEAD)"
if [[ "$resolved_head" != "$REVISION" ]]; then
  echo "circomlib checkout mismatch: expected $REVISION, got $resolved_head" >&2
  exit 1
fi

echo "circomlib fetched at $REVISION"
