//! The wire decoder.
//!
//! Every read is bounds-checked and every malformed input yields a typed error. There
//! are no panicking paths: this parses bytes from a network socket, so malformed input
//! is an expected condition rather than an exceptional one, and a panic here would
//! take down a subsystem that must never stop.

use crate::event::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
use sankhya_types::{Lsn, Timestamp};
use std::sync::Arc;

/// Microseconds between the Unix epoch and 2000-01-01, the source's own epoch.
const SOURCE_EPOCH_OFFSET_MICROS: i64 = 946_684_800_000_000;

/// What can go wrong while decoding. Each variant names the position, so a fuzz
/// finding or a production log line points at the exact byte.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// The message ended before the field did.
    Truncated { at: usize, needed: usize, available: usize },
    /// The leading message tag is not one this protocol version defines.
    UnknownMessage { tag: u8 },
    /// A column value marker other than the defined set.
    UnknownTupleKind { at: usize, kind: u8 },
    /// A tuple section marker other than the defined set.
    UnknownTupleSection { at: usize, marker: u8 },
    /// A replica-identity byte outside the defined set.
    UnknownReplicaIdentity { at: usize, value: u8 },
    /// A string field was not valid UTF-8.
    InvalidUtf8 { at: usize },
    /// A string field was not terminated.
    UnterminatedString { at: usize },
    /// A length field is negative where it may not be.
    NegativeLength { at: usize, value: i32 },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { at, needed, available } => {
                write!(f, "truncated at byte {at}: needed {needed}, {available} available")
            }
            Self::UnknownMessage { tag } => {
                write!(f, "unknown message tag {tag:#04x} ({:?})", *tag as char)
            }
            Self::UnknownTupleKind { at, kind } => {
                write!(f, "unknown value marker {kind:#04x} at byte {at}")
            }
            Self::UnknownTupleSection { at, marker } => {
                write!(f, "unknown tuple section {marker:#04x} at byte {at}")
            }
            Self::UnknownReplicaIdentity { at, value } => {
                write!(f, "unknown replica identity {value:#04x} at byte {at}")
            }
            Self::InvalidUtf8 { at } => write!(f, "invalid UTF-8 at byte {at}"),
            Self::UnterminatedString { at } => write!(f, "unterminated string at byte {at}"),
            Self::NegativeLength { at, value } => {
                write!(f, "negative length {value} at byte {at}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A bounds-checked cursor. Every accessor returns a `Result`; none can panic.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated {
            at: self.pos,
            needed: n,
            available: 0,
        })?;
        let slice = self.bytes.get(self.pos..end).ok_or(DecodeError::Truncated {
            at: self.pos,
            needed: n,
            available: self.bytes.len().saturating_sub(self.pos),
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        self.take(1)?.first().copied().ok_or(DecodeError::Truncated {
            at: self.pos,
            needed: 1,
            available: 0,
        })
    }

    fn i16(&mut self) -> Result<i16, DecodeError> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(self.u32()? as i32)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(self.u64()? as i64)
    }

    fn lsn(&mut self) -> Result<Lsn, DecodeError> {
        Ok(Lsn::new(self.u64()?))
    }

    /// Source timestamps are microseconds since 2000-01-01; SANKHYA uses the Unix
    /// epoch throughout, so the conversion happens here, once, at the boundary.
    fn timestamp(&mut self) -> Result<Timestamp, DecodeError> {
        Ok(Timestamp::from_micros(
            self.i64()?.saturating_add(SOURCE_EPOCH_OFFSET_MICROS),
        ))
    }

    fn cstring(&mut self) -> Result<String, DecodeError> {
        let start = self.pos;
        let rest = self.bytes.get(start..).ok_or(DecodeError::UnterminatedString { at: start })?;
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or(DecodeError::UnterminatedString { at: start })?;
        let text = std::str::from_utf8(&rest[..nul])
            .map_err(|_| DecodeError::InvalidUtf8 { at: start })?
            .to_owned();
        self.pos = start.saturating_add(nul).saturating_add(1);
        Ok(text)
    }
}

/// Decodes protocol messages. Stateless: relation descriptions are returned to the
/// caller rather than cached here, so the decoder itself has nothing to get out of
/// sync and can be reused or discarded freely.
#[derive(Clone, Copy, Debug, Default)]
pub struct Decoder;

impl Decoder {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Decode one message, requiring it to consume the whole slice.
    ///
    /// # Errors
    ///
    /// Returns a [`DecodeError`] naming the byte position for any malformed input.
    /// Never panics, for any input whatsoever — asserted by a fuzz-style property test.
    pub fn decode(&self, bytes: &[u8]) -> Result<Message, DecodeError> {
        self.decode_prefix(bytes).map(|(message, _)| message)
    }

    /// Decode the message at the start of `bytes`, returning it and the number of
    /// bytes it occupied.
    ///
    /// Messages are self-delimiting rather than length-prefixed, so a continuous
    /// replication stream can only be split by decoding it. This is the entry point
    /// the capture loop uses.
    ///
    /// # Errors
    ///
    /// As [`Decoder::decode`].
    pub fn decode_prefix(&self, bytes: &[u8]) -> Result<(Message, usize), DecodeError> {
        let mut c = Cursor::new(bytes);
        let message = Self::decode_at(&mut c)?;
        Ok((message, c.pos))
    }

    fn decode_at(c: &mut Cursor<'_>) -> Result<Message, DecodeError> {
        let tag = c.u8()?;
        match tag {
            b'B' => Ok(Message::Begin {
                final_lsn: c.lsn()?,
                commit_time: c.timestamp()?,
                xid: c.u32()?,
            }),
            b'C' => {
                let _flags = c.u8()?;
                Ok(Message::Commit {
                    commit_lsn: c.lsn()?,
                    end_lsn: c.lsn()?,
                    commit_time: c.timestamp()?,
                })
            }
            b'R' => Self::relation(c),
            b'Y' => Ok(Message::Type {
                type_oid: c.u32()?,
                namespace: c.cstring()?,
                name: c.cstring()?,
            }),
            b'I' => {
                let relation_id = c.u32()?;
                let marker = c.u8()?;
                if marker != b'N' {
                    return Err(DecodeError::UnknownTupleSection { at: c.pos - 1, marker });
                }
                Ok(Message::Insert { relation_id, new: Self::tuple(c)? })
            }
            b'U' => Self::update(c),
            b'D' => Self::delete(c),
            b'T' => Self::truncate(c),
            b'O' => Ok(Message::Origin { commit_lsn: c.lsn()?, name: c.cstring()? }),
            b'M' => Self::logical(c),
            b'S' => Ok(Message::StreamStart { xid: c.u32()?, first_segment: c.u8()? != 0 }),
            b'E' => Ok(Message::StreamStop),
            b'c' => {
                let xid = c.u32()?;
                let _flags = c.u8()?;
                Ok(Message::StreamCommit {
                    xid,
                    commit_lsn: c.lsn()?,
                    end_lsn: c.lsn()?,
                    commit_time: c.timestamp()?,
                })
            }
            b'A' => Ok(Message::StreamAbort {
                xid: c.u32()?,
                subtransaction_xid: c.u32()?,
            }),
            other => Err(DecodeError::UnknownMessage { tag: other }),
        }
    }

    fn relation(c: &mut Cursor<'_>) -> Result<Message, DecodeError> {
        let relation_id = c.u32()?;
        let namespace = c.cstring()?;
        let name = c.cstring()?;
        let identity_at = c.pos;
        let identity_byte = c.u8()?;
        let replica_identity = ReplicaIdentity::from_byte(identity_byte).ok_or(
            DecodeError::UnknownReplicaIdentity { at: identity_at, value: identity_byte },
        )?;
        let count = c.i16()?;
        if count < 0 {
            return Err(DecodeError::NegativeLength { at: c.pos - 2, value: i32::from(count) });
        }
        let mut columns = Vec::with_capacity(usize::try_from(count).unwrap_or(0).min(4096));
        for _ in 0..count {
            let flags = c.u8()?;
            columns.push(ColumnDescriptor {
                is_key: flags & 1 == 1,
                name: c.cstring()?,
                type_oid: c.u32()?,
                type_modifier: c.i32()?,
            });
        }
        Ok(Message::Relation(Arc::new(RelationDescriptor {
            relation_id,
            namespace,
            name,
            replica_identity,
            columns,
        })))
    }

    fn update(c: &mut Cursor<'_>) -> Result<Message, DecodeError> {
        let relation_id = c.u32()?;
        let marker_at = c.pos;
        let marker = c.u8()?;
        let (old, key_only) = match marker {
            // Key-only before-image: enough to identify the row, not to reconstruct it.
            b'K' => (Some(Self::tuple(c)?), true),
            // Full before-image, available only under the `full` replica identity.
            b'O' => (Some(Self::tuple(c)?), false),
            // No before-image at all.
            b'N' => {
                return Ok(Message::Update {
                    relation_id,
                    old: None,
                    key_only: false,
                    new: Self::tuple(c)?,
                })
            }
            other => return Err(DecodeError::UnknownTupleSection { at: marker_at, marker: other }),
        };
        let new_at = c.pos;
        let new_marker = c.u8()?;
        if new_marker != b'N' {
            return Err(DecodeError::UnknownTupleSection { at: new_at, marker: new_marker });
        }
        Ok(Message::Update { relation_id, old, key_only, new: Self::tuple(c)? })
    }

    fn delete(c: &mut Cursor<'_>) -> Result<Message, DecodeError> {
        let relation_id = c.u32()?;
        let marker_at = c.pos;
        let marker = c.u8()?;
        let key_only = match marker {
            b'K' => true,
            b'O' => false,
            other => return Err(DecodeError::UnknownTupleSection { at: marker_at, marker: other }),
        };
        Ok(Message::Delete { relation_id, old: Self::tuple(c)?, key_only })
    }

    fn truncate(c: &mut Cursor<'_>) -> Result<Message, DecodeError> {
        let count = c.i32()?;
        if count < 0 {
            return Err(DecodeError::NegativeLength { at: c.pos - 4, value: count });
        }
        let options = c.u8()?;
        let mut relation_ids = Vec::with_capacity(usize::try_from(count).unwrap_or(0).min(65_536));
        for _ in 0..count {
            relation_ids.push(c.u32()?);
        }
        Ok(Message::Truncate {
            relation_ids,
            cascade: options & 1 == 1,
            restart_identity: options & 2 == 2,
        })
    }

    fn logical(c: &mut Cursor<'_>) -> Result<Message, DecodeError> {
        let transactional = c.u8()? != 0;
        let lsn = c.lsn()?;
        let prefix = c.cstring()?;
        let len = c.i32()?;
        if len < 0 {
            return Err(DecodeError::NegativeLength { at: c.pos - 4, value: len });
        }
        let content = c.take(usize::try_from(len).unwrap_or(0))?.to_vec();
        Ok(Message::Logical { transactional, lsn, prefix, content })
    }

    fn tuple(c: &mut Cursor<'_>) -> Result<TupleData, DecodeError> {
        let count = c.i16()?;
        if count < 0 {
            return Err(DecodeError::NegativeLength { at: c.pos - 2, value: i32::from(count) });
        }
        let mut values = Vec::with_capacity(usize::try_from(count).unwrap_or(0).min(4096));
        for _ in 0..count {
            let kind_at = c.pos;
            let kind = c.u8()?;
            values.push(match kind {
                b'n' => TupleValue::Null,
                // The dangerous one. See TupleValue::Unchanged.
                b'u' => TupleValue::Unchanged,
                b't' => {
                    let len = c.i32()?;
                    if len < 0 {
                        return Err(DecodeError::NegativeLength { at: c.pos - 4, value: len });
                    }
                    let raw = c.take(usize::try_from(len).unwrap_or(0))?;
                    TupleValue::Text(
                        std::str::from_utf8(raw)
                            .map_err(|_| DecodeError::InvalidUtf8 { at: kind_at })?
                            .to_owned(),
                    )
                }
                b'b' => {
                    let len = c.i32()?;
                    if len < 0 {
                        return Err(DecodeError::NegativeLength { at: c.pos - 4, value: len });
                    }
                    TupleValue::Binary(c.take(usize::try_from(len).unwrap_or(0))?.to_vec())
                }
                other => return Err(DecodeError::UnknownTupleKind { at: kind_at, kind: other }),
            });
        }
        Ok(TupleData { values })
    }
}
