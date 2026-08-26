# Vendored PostgreSQL

| | |
|---|---|
| **Version** | 17.11 |
| **Source** | `https://ftp.postgresql.org/pub/source/v17.11/postgresql-17.11.tar.bz2` |
| **Retrieved** | 2026-08-25 |
| **Integrity** | `postgresql-17.11.tar.bz2.sha256`, upstream-published, verified on download and re-verified at every build |
| **Licence** | PostgreSQL Licence (permissive, BSD-style). Attribution required; see `COPYRIGHT` inside the archive |

## Why 17 or later

PostgreSQL 17 introduced failover-capable logical replication slots. Without them a
routine database failover destroys the replication slot and forces a full re-snapshot
of every replicated table — a multi-hour outage of the analytical tier triggered by an
ordinary availability event. See `REQUIREMENTS.md` `DEC-02`.

## Why the archive rather than an expanded tree

The compressed archive plus an upstream checksum is committed; the expanded source
(149 MB) is not. This is a deliberate trade:

- **Reproducibility is identical.** The checksum pins the exact bytes, verified at
  every build. An expanded tree adds no guarantee the checksum does not already give.
- **Repository size stays sane.** 21 MB against 149 MB, and the expanded tree would
  dominate every clone and every diff.
- **Auditability is preserved.** `build.sh` extracts to a known location; the source is
  one command away and its integrity is machine-verified.

If SANKHYA ever needs to *patch* PostgreSQL, this decision is revisited: patches
belong in version control alongside an expanded tree, and the change is one line in
`build.sh`.

## Building

```
vendor/postgresql/build.sh [--prefix DIR] [--jobs N]
```

Verifies the checksum, configures, builds and installs. Output is the supervised
child process SANKHYA manages in managed mode — see `ARCHITECTURE.md` §3.4.

Configured deliberately **without** Perl, Python and Tcl: SANKHYA never executes
untrusted procedural code inside the database, and excluding those interpreters
removes them from the attack surface and from the dependency matrix.
