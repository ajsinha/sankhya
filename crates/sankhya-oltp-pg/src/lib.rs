//! PostgreSQL lifecycle, supervision, pooling and migrations.
//!
//! **Empty by intent, and dated.** Nothing in this workspace supervises a PostgreSQL process
//! today --- `sankhya-api-pg` speaks the wire *protocol* to clients, which is the opposite
//! direction and a common thing to confuse the two for.
//!
//! **Scheduled: M8 §12.2**, where it stops being optional: leader election runs *through the
//! transactional store*, so there has to be a store, supervised, with a lifecycle somebody owns.
//!
//! The distinction from `sankhya-api-pg` is worth keeping in view when reading either:
//! that crate is how clients talk to SANKHYA, this one is how SANKHYA runs Postgres.
