//! Verifying a password against a stored verifier, and nothing else.
//!
//! # Why this exists
//!
//! It did not, and the consequence was `SEC-01`: **no password was ever verified**. The entire
//! check was that one had been *presented* and was non-empty. There was no credential store, no
//! hash and no comparison anywhere in the workspace, and because the username is self-asserted,
//! any client connected as any user by sending any byte string.
//!
//! Phase 0.8 of the remediation disclosed that in the startup line and the documentation rather
//! than leaving it implied. This is the repair.
//!
//! # What a verifier is
//!
//! A single line, storable in a configuration file and safe to read over somebody's shoulder:
//!
//! ```text
//! pbkdf2-sha256$600000$<salt-base64>$<derived-base64>
//! ```
//!
//! Four fields, `$`-separated, in the shape PostgreSQL's own SCRAM verifier uses — scheme,
//! iteration count, salt, derived key. The scheme and the count are written down rather than
//! assumed, because a verifier that does not say how it was made cannot be re-verified after
//! the default changes, and changing the default is the thing that will happen.
//!
//! # Why the count is in the file rather than in the code
//!
//! An operator raising it must not invalidate every existing verifier. Reading it from the
//! stored line means old verifiers keep working at the count they were made with, and new ones
//! are made at the current default — which is what lets the default move at all.
//!
//! # What this deliberately is not
//!
//! Not SCRAM. PostgreSQL's SCRAM-SHA-256 is a challenge-response exchange that never sends the
//! password, and it is the right destination. This verifies a password the client sent in
//! cleartext over the wire, which is why the transport-security posture is printed at startup
//! and why the documentation says what it says. Getting from *never verified* to *verified
//! against a stored key* is the step that closes `SEC-01`; getting from *cleartext over TLS* to
//! *challenge-response* is a protocol change and a separate one.

use std::num::NonZeroU32;

/// A stored verifier: what a password is checked against.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Verifier {
    iterations: NonZeroU32,
    salt: Vec<u8>,
    derived: Vec<u8>,
}

/// Why a verifier could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Malformed {
    /// Not four `$`-separated fields.
    Shape,
    /// A scheme this build does not implement.
    ///
    /// Named rather than folded into [`Self::Shape`]: a verifier written by a later version is
    /// a deployment that needs upgrading, and a verifier written by hand with a typo is a
    /// mistake. Telling them apart is the difference between an operator reading the release
    /// notes and an operator reading their own file.
    Scheme(String),
    /// The iteration count is not a positive number.
    Iterations,
    /// The salt or the derived key is not valid base64.
    Encoding,
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape => f.write_str(
                "a verifier is four `$`-separated fields: \
                 `pbkdf2-sha256$<iterations>$<salt>$<key>`",
            ),
            Self::Scheme(named) => write!(
                f,
                "`{named}` is not a password scheme this build knows. This one writes \
                 `pbkdf2-sha256`; a verifier naming something else was written by a different \
                 version"
            ),
            Self::Iterations => f.write_str("the iteration count must be a positive number"),
            Self::Encoding => f.write_str("the salt and the key are base64"),
        }
    }
}

impl std::error::Error for Malformed {}

/// The scheme this build writes.
const SCHEME: &str = "pbkdf2-sha256";

/// How many iterations a **new** verifier is made with.
///
/// OWASP's 2023 guidance for PBKDF2-HMAC-SHA256. Deliberately a number rather than a
/// calibration: a count derived from how fast the machine that happened to run the command is
/// gives a weaker verifier on a slower laptop, which is the wrong way round.
pub const ITERATIONS: u32 = 600_000;

/// How many bytes of salt and of derived key.
///
/// Thirty-two of each: the salt is the width of the hash it feeds, and a derived key shorter
/// than the hash throws away work already done.
const WIDTH: usize = 32;

impl Verifier {
    /// Read a stored verifier.
    ///
    /// # Errors
    ///
    /// [`Malformed`] for anything that is not one, with the reason distinguished — see the
    /// variants.
    pub fn parse(text: &str) -> Result<Self, Malformed> {
        let mut fields = text.trim().split('$');
        let (Some(scheme), Some(iterations), Some(salt), Some(derived), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(Malformed::Shape);
        };
        if scheme != SCHEME {
            return Err(Malformed::Scheme(scheme.to_string()));
        }
        let iterations = iterations
            .parse::<u32>()
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or(Malformed::Iterations)?;
        Ok(Self {
            iterations,
            salt: base64(salt).ok_or(Malformed::Encoding)?,
            derived: base64(derived).ok_or(Malformed::Encoding)?,
        })
    }

    /// Whether `password` is the one this verifier was made from.
    ///
    /// The comparison is `ring`'s, which is constant-time in the length it is given: a check
    /// that returns sooner for a password sharing a prefix with the right one leaks the right
    /// one, one byte at a time, to anybody who can measure it.
    #[must_use]
    pub fn verifies(&self, password: &[u8]) -> bool {
        ring::pbkdf2::verify(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            self.iterations,
            &self.salt,
            password,
            &self.derived,
        )
        .is_ok()
    }

    /// How many iterations this verifier was made with.
    ///
    /// Exposed so an operator can be told a stored verifier is weaker than the current default
    /// — which is the only way a raised default ever reaches the credentials already written
    /// down.
    #[must_use]
    pub const fn iterations(&self) -> u32 {
        self.iterations.get()
    }
}

/// Make a verifier for `password`, with `salt` bytes of randomness the caller supplies.
///
/// # Why the salt is a parameter
///
/// So this function is a pure one and its output is checkable. A function that reaches for the
/// system's randomness cannot be tested against a known vector, and the thing most worth
/// testing here is that the derivation matches a value computed by something else.
///
/// The salt must be unpredictable and unique per verifier; [`fresh_salt`] is what a caller
/// should use to get one.
///
/// # Errors
///
/// Returns `None` for an empty salt, which would make every verifier of one password identical
/// and defeat the point of having one.
#[must_use]
pub fn make(password: &[u8], salt: &[u8], iterations: u32) -> Option<String> {
    let iterations = NonZeroU32::new(iterations)?;
    if salt.is_empty() {
        return None;
    }
    let mut derived = [0u8; WIDTH];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password,
        &mut derived,
    );
    Some(format!(
        "{SCHEME}${iterations}${}${}",
        to_base64(salt),
        to_base64(&derived)
    ))
}

/// Salt bytes from the system's randomness.
///
/// # Errors
///
/// Returns `None` if the system cannot supply randomness, which is a machine that must not be
/// used to make a credential rather than one that should get a predictable salt.
#[must_use]
pub fn fresh_salt() -> Option<Vec<u8>> {
    use ring::rand::SecureRandom;
    let mut salt = vec![0u8; WIDTH];
    ring::rand::SystemRandom::new().fill(&mut salt).ok()?;
    Some(salt)
}

/// Standard base64, encoded. Written here rather than taken as a dependency: it is twenty lines
/// and this crate exists to have a small surface.
fn to_base64(bytes: &[u8]) -> String {
    // A match rather than an indexed alphabet, so the mapping is **total**: a lookup into a
    // 64-entry table needs a bound the reader has to check for themselves, and the workspace
    // denies indexing for exactly that reason. It also mirrors the decoder below, which is a
    // match for the same reason — and two halves of one encoding written in one shape are two
    // halves that can be read against each other.
    let sextet = |value: u32| -> char {
        match value {
            0..=25 => char::from(b'A'.wrapping_add(u8::try_from(value).unwrap_or(0))),
            26..=51 => char::from(b'a'.wrapping_add(u8::try_from(value - 26).unwrap_or(0))),
            52..=61 => char::from(b'0'.wrapping_add(u8::try_from(value - 52).unwrap_or(0))),
            62 => '+',
            _ => '/',
        }
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let at = |index: usize| chunk.get(index).map_or(0, |byte| u32::from(*byte));
        let packed = (at(0) << 16) | (at(1) << 8) | at(2);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(sextet((packed >> (18 - index * 6)) & 0x3f));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64, decoded. `None` for anything that is not.
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let trimmed = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut packed: u32 = 0;
    let mut bits = 0;
    for byte in trimmed.bytes() {
        packed = (packed << 6) | value(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((packed >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}
