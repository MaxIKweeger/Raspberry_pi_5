#!/usr/bin/env bash
# Cross-build a76probe on the host, copy it to the Pi, run a subcommand there and fetch results.
#
#   A76_PI_HOST=<ip> A76_PI_PASS=... ./deploy.sh selftest --core 1 --repeat 30
#
# Environment: A76_PI_HOST (required), A76_PI_USER (default hugues), A76_PI_PASS (required).
# Requires PuTTY's plink/pscp on PATH. The password is never stored in the repository.
set -euo pipefail

: "${A76_PI_HOST:?set A76_PI_HOST (Pi address)}"
HOST="$A76_PI_HOST"
USER_="${A76_PI_USER:-hugues}"
: "${A76_PI_PASS:?set A76_PI_PASS (Pi password)}"
TARGET=aarch64-unknown-linux-musl
REMOTE_DIR='~/a76probe'

cd "$(dirname "$0")"
cargo build --release
BIN="target/$TARGET/release/a76probe"

plink -batch -ssh -pw "$A76_PI_PASS" "$USER_@$HOST" "mkdir -p $REMOTE_DIR"
pscp -batch -q -pw "$A76_PI_PASS" "$BIN" "$USER_@$HOST:a76probe/a76probe"
plink -batch -ssh -pw "$A76_PI_PASS" "$USER_@$HOST" "cd $REMOTE_DIR && chmod +x a76probe && ./a76probe $*"

if [ "${1:-}" != "env" ]; then
  mkdir -p results
  pscp -batch -q -r -pw "$A76_PI_PASS" "$USER_@$HOST:a76probe/results/*" results/ || true
fi
