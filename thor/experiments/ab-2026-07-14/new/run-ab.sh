#!/usr/bin/env bash
# THOR A/B sandbox runner - FULLY ISOLATED from the live store / NAS / hooks.
#
#   - Source     = this clone (scratchpad/thor-ab), its own .git.
#   - Data       = scratchpad/ab-home/thor (a COPY of the live store + a copy of
#                  the ONNX model). The live %LOCALAPPDATA%\thor is never opened.
#   - This script redirects LOCALAPPDATA to the sandbox copy for the eval process
#     ONLY, so nothing outside this command is affected. No ship, no hooks, no MCP.
#
# Usage:
#   ./run-ab.sh recall                         # recall_eval (lambda sweep) baseline
#   THOR_EXP_RECENCY=0.3 ./run-ab.sh recall    # recall with a candidate flag on
#   ./run-ab.sh drift-live                      # drift eval against the store copy
#   THOR_EXP_LAMBDA=3.0 ./run-ab.sh drift-live
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export LOCALAPPDATA="$(cd "$HERE/../ab-home" && pwd)"   # data dir -> sandbox copy
BIN="$HERE/thor/target/release/examples"
case "${1:-recall}" in
  recall)     shift; "$BIN/recall_eval.exe" "$@" ;;
  drift)      shift; "$BIN/drift_eval.exe" "$@" ;;
  drift-live) shift; "$BIN/drift_eval.exe" --live "$LOCALAPPDATA/thor/eval/drift_scenarios.json" "$@" ;;
  *) echo "usage: run-ab.sh [recall|drift|drift-live]  (candidate flags via THOR_EXP_*=... prefix)"; exit 1 ;;
esac
