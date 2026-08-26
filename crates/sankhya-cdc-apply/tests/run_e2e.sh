#!/usr/bin/env bash
# Run the end-to-end capture test against the local test database.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
export SANKHYA_PG_BIN="$ROOT/.build/pg-install/bin"
export SANKHYA_E2E_SOCKET="${SANKHYA_E2E_SOCKET:-/tmp/sankhya-test-sock}"
[[ -x "$SANKHYA_PG_BIN/psql" ]] || { echo "build PostgreSQL first" >&2; exit 1; }
exec cargo test -p sankhya-cdc-apply --test e2e_real_capture -- --nocapture
