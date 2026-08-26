#!/usr/bin/env bash
# Regenerate the real-PostgreSQL conformance fixture.
#
# The fixture exists so the decoder is validated against bytes produced by an actual
# server rather than by our own encoder. Two traps are encoded in this script, both
# discovered the hard way:
#
#  1. Capture uses pg_logical_slot_peek_binary_changes, which returns one row per
#     message with exact bytes. It does NOT use `pg_recvlogical -f`, which appends a
#     newline after each message because it is built for textual output plugins. On a
#     binary stream those newlines are indistinguishable from payload.
#
#  2. The large value must be genuinely stored out-of-line, or the server never
#     withholds it and the most important assertion in the suite silently passes for
#     the wrong reason. `repeat('x', 12000)` compresses ~80x and stays inline, so the
#     column is set to EXTERNAL storage and filled with incompressible random data.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
PG="$ROOT/.build/pg-install/bin"
DATA="$ROOT/.build/fixture-pg"
SOCK="/tmp/sankhya-fixture-sock"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE/real-pgoutput-v4.bin"

[[ -x "$PG/initdb" ]] || { echo "build PostgreSQL first: vendor/postgresql/build.sh" >&2; exit 1; }

"$PG/pg_ctl" -D "$DATA" stop -m immediate >/dev/null 2>&1 || true
rm -rf "$DATA" "$SOCK"; mkdir -p "$DATA" "$SOCK"
"$PG/initdb" -D "$DATA" -U sankhya --auth=trust -E UTF8 --no-sync >/dev/null
cat >> "$DATA/postgresql.conf" <<CONF
wal_level = logical
max_replication_slots = 8
max_wal_senders = 8
listen_addresses = ''
unix_socket_directories = '$SOCK'
fsync = off
CONF
"$PG/pg_ctl" -D "$DATA" -l "$DATA/pg.log" start -w -t 30 >/dev/null
trap '"$PG/pg_ctl" -D "$DATA" stop -m immediate >/dev/null 2>&1 || true' EXIT

psql() { "$PG/psql" -h "$SOCK" -U sankhya -d postgres "$@"; }

psql -q <<'SQL'
CREATE TABLE device_readings (
    id          bigserial PRIMARY KEY,
    device_id   text NOT NULL,
    reading     numeric(12,4) NOT NULL,
    payload     text,
    observed_at timestamptz NOT NULL DEFAULT now()
);
-- EXTERNAL disables compression, so the value is stored out-of-line and the server
-- withholds it on an update that does not touch it.
ALTER TABLE device_readings ALTER COLUMN payload SET STORAGE EXTERNAL;
CREATE PUBLICATION sankhya_all FOR ALL TABLES;
SQL

psql -tAc "SELECT pg_create_logical_replication_slot('sankhya_fixture','pgoutput');" >/dev/null

psql -q <<'SQL'
-- Incompressible: 400 concatenated md5 digests, ~12.8 KB, defeats the compressor.
INSERT INTO device_readings (device_id, reading, payload)
SELECT 'sensor-1', 21.5500, string_agg(md5(g::text || random()::text), '')
FROM generate_series(1, 400) g;

INSERT INTO device_readings (device_id, reading) VALUES ('sensor-2', 19.2500);

-- Deliberately does NOT touch payload: this is what produces the unchanged marker.
UPDATE device_readings SET reading = 22.0000 WHERE device_id = 'sensor-1';

DELETE FROM device_readings WHERE device_id = 'sensor-2';
SQL

# Fail loudly if the value did not actually go out-of-line, rather than producing a
# fixture whose central assertion passes vacuously.
inline_size=$(psql -tAc "SELECT pg_column_size(payload) FROM device_readings WHERE device_id='sensor-1';")
if (( inline_size < 2000 )); then
  echo "FIXTURE INVALID: payload is only $inline_size bytes on the page, so it was not" >&2
  echo "stored out-of-line and no unchanged marker will be produced." >&2
  exit 1
fi

psql -tA -c "SELECT encode(data,'hex') FROM pg_logical_slot_peek_binary_changes(
    'sankhya_fixture', NULL, NULL, 'proto_version','4','publication_names','sankhya_all');" \
  | python3 -c "
import struct, sys
out = bytearray(); n = 0
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    b = bytes.fromhex(line)
    out += struct.pack('>I', len(b)) + b
    n += 1
open('$OUT','wb').write(out)
print(f'wrote $OUT: {n} messages, {len(out)} bytes')
"
