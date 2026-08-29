//! Control plane and graph API over gRPC.
//!
//! **Empty by intent, and dated.** This is M6's exit criterion 7, carried into M8 by owner
//! decision on 2026-08-28: the size decision and the route table are built and tested in
//! `sankhya-api-flight`; the gRPC transport and every write path are not. Until they are, this
//! crate holds no code rather than a stub that would look like progress.
//!
//! **Scheduled: M8 §12.2.** If M8 closes without it, the criterion is still unmet and must be
//! carried again explicitly --- not quietly absorbed, which is how an exit criterion becomes a
//! decoration.
