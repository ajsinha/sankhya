//! The frame the parent and the worker exchange.
//!
//! # Why binary and length-prefixed
//!
//! Because the values are a buffer. `ADR-0010` requires a **batch** to cross the boundary
//! rather than a row --- an interpreter boundary crossed per row is crossed a hundred million
//! times in a scan --- and a batch of doubles is not text.
//!
//! # Why not Arrow IPC, which `ADR-0010` named
//!
//! Its reason was that the data is already Arrow on both sides and should cross zero-copy. The
//! premise holds; the *packaging* did not survive contact with a machine. Reading Arrow IPC in
//! Python needs `pyarrow`, so requiring it would make a user's aggregation unavailable on any
//! machine that has Python and not that package --- and the feature would be untestable on the
//! one machine this repository is built on.
//!
//! What crosses instead is **Arrow's own buffer layout without the IPC envelope**: the values
//! as little-endian `f64`, and the validity bitmap beside them. NumPy reads that with
//! `frombuffer` at no copy, which is exactly the path Arrow would have given for a `Float64`
//! column; the standard library reads it as a `memoryview` cast to doubles, without
//! materialising a list. The property `ADR-0010` wanted --- the boundary is crossed per batch
//! and the work is done in vectorised code --- is kept. The dependency is not.

/// The magic word, so a reply from something that is not the worker is not read as one.
pub(crate) const MAGIC: u32 = 0x474B_534B;
/// The protocol's version. A worker that speaks another says so rather than guessing.
pub(crate) const VERSION: u16 = 1;

pub(crate) const ACCUMULATE: u16 = 1;
pub(crate) const MERGE: u16 = 2;
pub(crate) const FINISH: u16 = 3;
pub(crate) const DESCRIBE: u16 = 4;

pub(crate) const IS_STATE: u16 = 1;
pub(crate) const IS_NUMBER: u16 = 2;
pub(crate) const IS_REFUSAL: u16 = 3;

/// Why a call did not produce an answer.
///
/// Every variant is something to tell a person about *their function*, which is why none of
/// them is an `io::Error`: "broken pipe" is true and useless, and the sentence somebody needs
/// is which of their four methods refused and what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The machine cannot host the boundary the function must run behind.
    NoBoundary(String),
    /// The function itself refused, or raised. The message is the worker's.
    Function(String),
    /// It ran past a bound.
    Bound(String),
    /// The worker answered something this parent cannot read.
    Protocol(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBoundary(said) => write!(f, "{said}"),
            Self::Function(said) => write!(f, "{said}"),
            Self::Bound(said) => write!(f, "{said}"),
            Self::Protocol(said) => write!(f, "the worker answered unreadably: {said}"),
        }
    }
}

impl std::error::Error for Refused {}

/// One request, framed.
///
/// Always four parts, empty where an operation does not use one. A fixed shape rather than a
/// variable one because the alternative is a length field somebody eventually reads as the
/// wrong part, and the empty parts cost eight bytes each.
pub(crate) fn request(operation: u16, parts: [&[u8]; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + parts.iter().map(|part| part.len()).sum::<usize>());
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&operation.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    for part in &parts {
        out.extend_from_slice(&(part.len() as u64).to_le_bytes());
    }
    for part in &parts {
        out.extend_from_slice(part);
    }
    out
}

/// One response, unframed.
///
/// # Errors
///
/// [`Refused::Protocol`] when the bytes are not a response, and [`Refused::Function`] when they
/// are a response saying the function refused.
pub(crate) fn response(bytes: &[u8]) -> Result<(u16, Vec<u8>), Refused> {
    let header = bytes
        .get(..16)
        .ok_or_else(|| Refused::Protocol(format!("{} byte(s), which is not a frame", bytes.len())))?;
    let word = |at: usize, width: usize| -> u64 {
        header
            .get(at..at + width)
            .map_or(0, |slice| slice.iter().rev().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
    };
    if u32::try_from(word(0, 4)).unwrap_or(0) != MAGIC {
        return Err(Refused::Protocol("it does not begin with this protocol's word".to_owned()));
    }
    if u16::try_from(word(4, 2)).unwrap_or(0) != VERSION {
        return Err(Refused::Protocol(format!(
            "it speaks version {}, and this server speaks {VERSION}",
            word(4, 2)
        )));
    }
    let kind = u16::try_from(word(6, 2)).unwrap_or(0);
    let length = usize::try_from(word(8, 8)).unwrap_or(0);
    let payload = bytes
        .get(16..16 + length)
        .ok_or_else(|| {
            Refused::Protocol(format!("it claims {length} byte(s) and carries fewer"))
        })?
        .to_vec();
    if kind == IS_REFUSAL {
        return Err(Refused::Function(String::from_utf8_lossy(&payload).into_owned()));
    }
    Ok((kind, payload))
}

/// A batch of values, as the worker reads them.
pub(crate) fn values(numbers: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(numbers.len() * 8);
    for number in numbers {
        out.extend_from_slice(&number.to_le_bytes());
    }
    out
}
