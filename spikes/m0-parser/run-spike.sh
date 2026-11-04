#!/usr/bin/env bash
# M0 parser spike evidence run.
#   ./run-spike.sh
# Produces evidence-corpus.log, evidence-wire.log, evidence-edge.log.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$HERE"

cargo build --release
BIN=target/release/m0-parser

echo "== synthetic corpus (queries/responses derived from the aether-proxy profile)"
"$BIN" | tee evidence-corpus.log

echo
echo "== real wire samples captured from chinadns-ng by the fixture harness"
"$BIN" wire ../m0-fixtures/wire-samples.json | tee evidence-wire.log

echo
echo "== edge-case wire samples (malformed / unusual queries)"
"$BIN" wire ../m0-fixtures/edge-wire-samples.json | tee evidence-edge.log
