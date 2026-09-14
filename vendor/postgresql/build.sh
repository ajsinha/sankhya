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
# `--without-icu`, deliberately, and it is the one interesting choice here.
#
# PostgreSQL 16+ links ICU by default, and a binary linked against the ICU that was installed
# when it was built stops loading the day the system moves --- which is exactly what happened
# on 2026-09-13: the machine went from ICU 74 to ICU 78, all four binaries were still present,
# and `initdb` failed with `libicuuc.so.74: cannot open shared object file`. Four tests went red
# naming a shared library rather than a stale build.
#
# This PostgreSQL exists to give the supervisor tests a cluster to supervise. It is not a
# deployment and nothing here depends on an ICU collation, so the dependency buys nothing and
# costs a rebuild every time a distribution bumps a soname. libc collation is the fallback and
# is what `initdb` will use.
#
# Restore `--with-icu` only alongside a reason that needs it, and expect to rebuild when the
# system ICU next moves.
./configure --prefix="$PREFIX" \
  --with-openssl --without-icu --with-readline --with-zlib \
  --without-perl --without-python --without-tcl \
  --enable-thread-safety > "$SRCDIR/configure.log" 2>&1

echo "==> building with $JOBS jobs"
make -j"$JOBS" -s > "$SRCDIR/make.log" 2>&1
make -s install > "$SRCDIR/install.log" 2>&1

echo "==> built $("$PREFIX/bin/postgres" --version)"
