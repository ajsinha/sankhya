#!/usr/bin/env bash
# Build the vendored PostgreSQL from verified source.
# Integrity is checked on every invocation, not only on download.
set -euo pipefail

PG_VERSION="17.11"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PREFIX="$ROOT/.build/pg-install"
JOBS="$(nproc 2>/dev/null || echo 4)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --jobs)   JOBS="$2";   shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

TARBALL="$HERE/postgresql-$PG_VERSION.tar.bz2"
SRCDIR="$ROOT/.build/pg-src"

echo "==> verifying integrity"
( cd "$HERE" && sha256sum -c "postgresql-$PG_VERSION.tar.bz2.sha256" )

if [[ -x "$PREFIX/bin/postgres" ]] \
   && "$PREFIX/bin/postgres" --version 2>/dev/null | grep -q "$PG_VERSION"; then
  echo "==> already built at $PREFIX"
  exit 0
fi

echo "==> extracting"
mkdir -p "$SRCDIR"
tar -xf "$TARBALL" -C "$SRCDIR"

echo "==> configuring (prefix $PREFIX)"
cd "$SRCDIR/postgresql-$PG_VERSION"
./configure --prefix="$PREFIX" \
  --with-openssl --with-icu --with-readline --with-zlib \
  --without-perl --without-python --without-tcl \
  --enable-thread-safety > "$SRCDIR/configure.log" 2>&1

echo "==> building with $JOBS jobs"
make -j"$JOBS" -s > "$SRCDIR/make.log" 2>&1
make -s install > "$SRCDIR/install.log" 2>&1

echo "==> built $("$PREFIX/bin/postgres" --version)"
