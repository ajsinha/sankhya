#!/usr/bin/env bash
# Start the single SANKHYA that an adversarial review runs against.
#
# One server, one warehouse, one schema per reviewer. This machine runs one SANKHYA and no
# `cargo` while a review is in progress: five reviewers each starting a server, or each
# compiling the workspace, would take the box down --- and a review that kills the machine it
# is reviewing has proved nothing.
#
# Sharing also makes the review harder, which is the point. Several clients writing and reading
# concurrently in several schemas is the isolation the server claims to provide, exercised by
# people trying to break it rather than by the person who wrote it.
#
# Usage:  tools/review-server.sh start|stop|status
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE="${SANKHYA_REVIEW_DIR:-/tmp/sankhya-review}"
PORT="${SANKHYA_REVIEW_PORT:-55432}"
METRICS="${SANKHYA_REVIEW_METRICS:-55433}"
BINARY="$ROOT/target/debug/sankhya-server"
LOG="$BASE/server.log"
PIDFILE="$BASE/server.pid"

# Stopped through the pidfile, never through a pattern match on the process table. A
# `pkill -f target/debug/sankhya-server` matches the shell that ran it, because that string is
# in its own command line --- which kills the caller instead of the server, once, mysteriously.
stop_it() {
  if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    rm -f "$PIDFILE"
    echo "stopped"
  else
    rm -f "$PIDFILE"
    echo "not running"
  fi
}

case "${1:-status}" in
  start)
    [[ -x "$BINARY" ]] || { echo "no binary at $BINARY -- build it first" >&2; exit 1; }
    if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      echo "already running as $(cat "$PIDFILE") on port $PORT"; exit 0
    fi
    mkdir -p "$BASE/config/feeds" "$BASE/spool" "$BASE/data" "$BASE/warehouse"
    [[ -f "$BASE/config/application.yaml" ]] || \
      echo '# review deployment' > "$BASE/config/application.yaml"

    SANKHYA_NO_PASSWORD=1 \
    SANKHYA_LISTEN="127.0.0.1:$PORT" \
    SANKHYA_METRICS_LISTEN="127.0.0.1:$METRICS" \
    SANKHYA_CONFIG="$BASE/config/application.yaml" \
    SANKHYA_WAREHOUSE="$BASE/warehouse" \
    SANKHYA_DATA_DIR="$BASE/data" \
    SANKHYA_FEED_INTERVAL_SECONDS=5 \
      nohup "$BINARY" > "$LOG" 2>&1 &
    echo $! > "$PIDFILE"

    # Waits for the banner rather than sleeping. A sleep long enough to be reliable is slow,
    # and one short enough not to be is a race that fails on a busy machine.
    for _ in $(seq 1 60); do
      grep -q "listening on" "$LOG" 2>/dev/null && break
      sleep 1
    done
    grep -q "listening on" "$LOG" 2>/dev/null || {
      echo "the server never announced a port; see $LOG" >&2
      exit 1
    }
    echo "started as $(cat "$PIDFILE") on port $PORT -- log $LOG"
    ;;
  stop) stop_it ;;
  status)
    if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      echo "running as $(cat "$PIDFILE") on port $PORT"
    else
      echo "not running"
    fi
    ;;
  *) echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
