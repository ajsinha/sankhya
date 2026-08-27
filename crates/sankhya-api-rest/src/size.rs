//! Deciding whether a result may be JSON.
//!
//! # The cap is not a resource guard
//!
//! `FR-API-06` states the reason, and it is a product reason rather than an operational one:
//!
//! > Bulk data SHALL NOT be offered over JSON. **Serializing analytical results as JSON
//! > destroys the zero-copy premise and defines published benchmarks downward.**
//!
//! The failure this prevents is not a server running out of memory. It is that REST is the
//! convenient surface, so people will use it for bulk extract *because* it is convenient —
//! and then measure the system through it. A columnar engine benchmarked through a JSON
//! encoder is a JSON encoder benchmark, and the number that gets published is that one.
//!
//! So the cap exists to stop the convenient path from becoming the measured path. That is
//! why it is hard rather than configurable upward, and why exceeding it is **not an error**.
//!
//! # It is a redirection, not a refusal
//!
//! A result too large for JSON comes back as a **Flight ticket**: the same query, already
//! planned and authorized, redeemable over the columnar path. `413 Payload Too Large` would
//! send somebody to ask for a bigger cap. A ticket sends them to the surface that was built
//! for what they are doing.
//!
//! # You cannot count the rows to decide whether to return the rows
//!
//! The decision is made from the plan's **estimate**, before anything is materialised.
//! Materialising a result in order to measure it is the cost the cap exists to avoid, so a
//! decision taken afterwards has already lost.
//!
//! Estimates are wrong, so there is a second guard: encoding stops the moment the *actual*
//! output passes the cap, and the response becomes a ticket. Bounded by the cap either way.
//! **Never a truncated body** — a JSON array cut short is either invalid, or worse, valid and
//! silently short, and a client cannot tell the difference from a small result.

use sankhya_api_flight::ticket::Ticket;
use sankhya_types::TenantId;
use std::fmt;

/// The largest response this gateway will encode as JSON.
///
/// One megabyte, and the figure is deliberately modest. It is sized for *"show me the last
/// twenty rows"*, which is what a REST surface is for, and not for anything a person would
/// call an extract. Somebody who finds it small is doing the thing this cap exists to
/// redirect.
pub const MAX_BYTES: usize = 1024 * 1024;

/// The most rows this gateway will encode as JSON.
///
/// A separate limit because a row can be arbitrarily large and a row count is what a caller
/// reasons about. Whichever is reached first decides.
pub const MAX_ROWS: u64 = 10_000;

/// What the plan expects to produce.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Estimate {
    /// Rows the plan expects.
    pub rows: u64,
    /// Bytes the plan expects, before encoding.
    pub bytes: u64,
}

/// How a result should be returned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// Small enough. Encode it.
    Inline,
    /// Too large for JSON. Redeem this over Flight.
    ///
    /// Carries *why*, because a client that is handed a ticket without being told what
    /// happened concludes the query failed.
    Redirect {
        /// The ticket, already authorized and bound to the tenant that asked.
        ticket: Ticket,
        /// What was too large, in the words a caller needs.
        why: String,
    },
}

impl Delivery {
    /// Whether the result may be encoded as JSON.
    #[must_use]
    pub const fn inline(&self) -> bool {
        matches!(self, Self::Inline)
    }
}

/// Decide before anything is materialised.
///
/// `issue` produces the ticket, and is only called when one is needed — issuing a ticket for
/// every request would put a redeemable credential into every small response, which is a
/// larger surface than the feature is worth.
pub fn deliver(
    estimate: Estimate,
    tenant: TenantId,
    statement: &str,
    issue: impl FnOnce() -> Ticket,
) -> Delivery {
    let _ = tenant;
    if estimate.rows > MAX_ROWS {
        return Delivery::Redirect {
            ticket: issue(),
            why: format!(
                "this query is estimated to return {} rows and this surface encodes at most \
                 {MAX_ROWS} as JSON. Redeem the ticket over Arrow Flight SQL, which streams \
                 the same result columnar and without materialising it. The limit is not a \
                 quota to be raised: serialising analytical results as JSON is the thing it \
                 exists to prevent",
                estimate.rows
            ),
        };
    }
    if estimate.bytes as usize > MAX_BYTES {
        return Delivery::Redirect {
            ticket: issue(),
            why: format!(
                "this query is estimated to return {} bytes and this surface encodes at most \
                 {MAX_BYTES} as JSON. Redeem the ticket over Arrow Flight SQL. The statement \
                 is `{}`, already planned and authorized",
                estimate.bytes,
                shorten(statement)
            ),
        };
    }
    Delivery::Inline
}

/// A statement, shortened for a message.
///
/// Query text is data --- `ARCHITECTURE` §17.1 --- so even here it is bounded rather than
/// echoed whole. A predicate carrying a customer's identifier does not belong in an error a
/// proxy might log.
fn shorten(statement: &str) -> String {
    const KEEP: usize = 60;
    if statement.chars().count() <= KEEP {
        return statement.to_string();
    }
    let head: String = statement.chars().take(KEEP).collect();
    format!("{head}…")
}

/// Tracks the response as it is encoded, and stops it exceeding the cap.
///
/// The second guard. An estimate can be wrong in the direction that matters --- a plan
/// expecting a hundred rows can produce a million --- and the first guard has already let
/// the request through by then.
#[derive(Debug)]
pub struct Budget {
    bytes: usize,
    rows: u64,
    exceeded: bool,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

impl Budget {
    /// A fresh budget.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: 0,
            rows: 0,
            exceeded: false,
        }
    }

    /// Account for one more encoded row.
    ///
    /// Returns whether encoding may continue. Once it says no it keeps saying no: a caller
    /// that ignores one refusal and asks again about a smaller row must not be told to carry
    /// on, because the response is already over.
    pub fn accept(&mut self, encoded_bytes: usize) -> bool {
        if self.exceeded {
            return false;
        }
        self.bytes = self.bytes.saturating_add(encoded_bytes);
        self.rows = self.rows.saturating_add(1);
        if self.bytes > MAX_BYTES || self.rows > MAX_ROWS {
            self.exceeded = true;
            return false;
        }
        true
    }

    /// Whether the response outgrew the cap while being encoded.
    #[must_use]
    pub const fn exceeded(&self) -> bool {
        self.exceeded
    }

    /// How many rows were accepted.
    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// How many bytes were accepted.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Why the encoded response has to be abandoned.
    ///
    /// **Abandoned, not truncated.** A JSON array cut short is either invalid, or --- worse
    /// --- valid and silently short, and a client cannot tell that from a small result.
    #[must_use]
    pub fn overrun(&self) -> Option<String> {
        self.exceeded.then(|| {
            format!(
                "the plan's estimate was low: this result passed {MAX_ROWS} rows or \
                 {MAX_BYTES} bytes while being encoded, at {} row(s) and {} byte(s). The \
                 partial response is discarded rather than sent — a JSON array cut short is \
                 either invalid or silently incomplete, and a client cannot tell the second \
                 from a small answer",
                self.rows, self.bytes
            )
        })
    }
}

impl fmt::Display for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline => f.write_str("encoded as JSON"),
            Self::Redirect { why, .. } => f.write_str(why),
        }
    }
}
