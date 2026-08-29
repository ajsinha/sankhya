//! Object-store construction, credentials, retries, caching, and the conformance probe.
//!
//! **Empty by intent, and dated.** Everything published today goes to a local filesystem.
//!
//! **Scheduled: M8 §12.1.** [ADR-0013](../../docs/adr/0013-concurrency-and-data-safety.md)
//! is what makes this concrete rather than aspirational: a commit claims its version with a
//! primitive that must *fail* when the name is taken, and on an object store that is a
//! conditional put --- `If-None-Match: *` for S3 and Azure, `ifGenerationMatch=0` for GCS. The
//! local `link(2)` and the remote conditional put are two spellings of one property, and
//! writing them side by side is what keeps each honest about the other.
//!
//! The conformance probe named above is the reason this is not merely a client wrapper: a store
//! that does not offer a conditional put cannot host a warehouse safely, and the system should
//! find that out at configuration time rather than during a race.
