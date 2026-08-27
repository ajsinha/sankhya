# ADR-0006 — Arrow Flight SQL as the bulk data plane

**Status:** Accepted · **Date:** 2026-08-27 · **Milestone:** M5 §9.6
**Implements:** `FR-API-01` (Flight SQL as the primary bulk data plane), `FR-API-07`
(demand-driven streaming)

## Context

The wire-protocol front door works and real clients use it. It is also a **row protocol**,
which means the last step of every query converts columnar Arrow batches into rows, one
value at a time, with a length prefix per value.

That conversion is forced by the client and is where columnar ends. For an interactive query
returning a screenful it costs nothing worth measuring. For a bulk extract it is the whole
cost: the data was columnar on disk, columnar in memory, columnar through every operator,
and is then taken apart to be put back together by the receiver.

`FR-API-01` therefore names Flight SQL the **primary bulk data plane**, and `FR-API-06` is
blunt about the alternative: *"Serializing analytical results as JSON destroys the zero-copy
premise and defines published benchmarks downward."* The same argument applies, more mildly,
to rows.

## Decision

Implement Flight SQL over gRPC, using `arrow-flight 59.2.0`.

### Why the dependency is cheap here and would not be elsewhere

`arrow-flight` is part of the Arrow project and versions with it. Pinning `=59.2.0` puts it
in the **same exact-pinned family** as `arrow`, `arrow-array`, `arrow-schema`, `arrow-ipc`
and `parquet`, which ADR-0001 already established must move together.

Measured rather than assumed: adding it brought **zero new duplicate crates**. The
`tonic`/`prost` stack it needs was not previously present and arrives cleanly. Had it
required a second Arrow generation — as a dataframe engine would — ADR-0001 would have
ruled it out.

### What is streamed and what is not

`FR-API-07` requires results to be demand-driven and forbids materialising a result set
server-side. DataFusion already produces a `SendableRecordBatchStream`, and Flight's
`do_get` returns a stream, so the two compose without an intermediate buffer: a batch is
encoded and sent as it is produced.

The consequence worth naming is that **a query's error can arrive mid-stream**. A row
protocol sends its error before the first row or not at all; a streaming one may have sent a
gigabyte before a decode failure on the last file. Clients must handle a stream that ends in
an error, and this one reports it as a `Status` on the stream rather than closing quietly ---
a truncated stream that ends cleanly is indistinguishable from a complete one.

### Authorization happens before the ticket is issued

Flight splits a query into `GetFlightInfo` (plan it, return a ticket) and `DoGet` (redeem
the ticket for data). That split is a security boundary and is easy to get wrong.

The decision is made at `GetFlightInfo`, and **the ticket carries the outcome**. A ticket is
not a query to be re-planned later; redeeming one does not re-authorize, because the
principal that redeems it may differ from the one that requested it if a ticket leaks. So a
ticket is bound to the tenant it was issued for and is refused if presented by another.

### What this does not do

No `DoPut`, so Flight is read-only here. Writing goes through the transactional store for a
managed table and through the publishing library for an external one, and adding a third
write path with its own semantics would be a way for those two to disagree.

No prepared statements, no transactions, no `DoExchange`. Each is a real part of the Flight
SQL surface and each is absent rather than half-present, because a client that discovers a
method returns `UNIMPLEMENTED` is better served than one that discovers it works differently
than it should.

## Consequences

- A bulk extract stays columnar from the Parquet page to the client's Arrow buffer.
- Two data planes now exist with different characteristics, and a client has to choose. The
  guidance is the boring one: the wire protocol for anything interactive or tool-driven,
  Flight for anything bulk or programmatic.
- Errors can arrive mid-stream, which is a client-visible behaviour change from the row
  protocol and is documented rather than discovered.

## Revisit if

A second Arrow generation ever enters the tree, at which point this dependency and every
other member of the pinned family are one decision rather than several.
