//! A name a statement supplied, checked before it becomes part of a path.
//!
//! # The failure this exists for
//!
//! Four places built a path as `warehouse.join(DIRECTORY).join(format!("{name}.json"))` from a
//! name a client typed, with no constraint on the name beyond being non-empty and free of
//! whitespace. `Path::join` **replaces the entire path when the component is absolute**, and
//! honours `..` when it is not, so:
//!
//! - `CREATE AGGREGATION /var/tmp/x LANGUAGE PYTHON AS $$…$$` writes wherever the server's user
//!   can write, and
//! - `DROP SNAPSHOT ../_cubes/regional` deletes a cube definition.
//!
//! The worst target is a snapshot document, and not because a snapshot is precious. An absent
//! snapshot pins nothing, so **deleting one releases the files the sweeper was holding back** —
//! the deletion the whole retention mechanism exists to prevent, reached through the name of a
//! `DROP` statement. `SEC-06`.
//!
//! # Why an allow-list and not a check for `..`
//!
//! Because a deny-list of dangerous shapes is a list somebody has to keep complete, and the
//! list is longer than it looks: `..`, a leading `/`, a bare `.`, a NUL byte, a Windows drive
//! letter, a trailing dot or space that Windows strips, a name that is a reserved device, a
//! name that differs from another only in case on a case-insensitive filesystem, a Unicode
//! character that normalises to a separator. Enumerating those is a research project with a
//! deadline attached.
//!
//! Saying what a name **may** contain is one line and has no tail. The cost is that a name
//! outside the set is refused rather than escaped, which is the choice this repository makes
//! everywhere else: refuse rather than substitute. An operator who wanted `sales/2024` can
//! write `sales_2024`; an operator who wanted `../../etc/passwd` can be refused.

use std::fmt;

/// The longest name that may become a file name.
///
/// Not a guess: 255 bytes is the limit on ext4, XFS, APFS and NTFS alike, and the four bytes of
/// `.json` come out of the same budget. A name refused here is refused with a reason; a name
/// allowed through and refused by the kernel is an `ENAMETOOLONG` in a log nobody is reading.
pub const LONGEST: usize = 200;

/// Why a name was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAName {
    /// It was empty.
    Empty,
    /// It was longer than [`LONGEST`] bytes.
    TooLong {
        /// How long it actually was.
        was: usize,
    },
    /// It contained something a name may not contain.
    Forbidden {
        /// The first character that is not permitted.
        character: char,
    },
    /// It was `.` or `..`, which name directories rather than documents.
    Relative,
}

impl fmt::Display for NotAName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "a name may not be empty"),
            Self::TooLong { was } => write!(
                f,
                "a name may be at most {LONGEST} bytes and this one is {was}"
            ),
            Self::Forbidden { character } => write!(
                f,
                "a name may hold letters, digits, `_`, `-` and `.`, and this one holds \
                 {character:?}"
            ),
            Self::Relative => write!(
                f,
                "`.` and `..` name a directory rather than a document, and a name that \
                 traverses is the defect this check exists for"
            ),
        }
    }
}

impl std::error::Error for NotAName {}

/// Whether this name may become one component of a path.
///
/// Letters, digits, `_`, `-` and `.`, with `.` and `..` refused outright. ASCII letters only:
/// a name is a file name on somebody else's filesystem eventually, and two names that differ
/// only by a Unicode normalisation form are one file on macOS and two on Linux — a difference
/// that shows up as a document silently overwritten rather than as an error.
///
/// # Errors
///
/// [`NotAName`] saying which rule the name broke, so the refusal can name it.
pub fn checked(name: &str) -> Result<&str, NotAName> {
    if name.is_empty() {
        return Err(NotAName::Empty);
    }
    if name.len() > LONGEST {
        return Err(NotAName::TooLong { was: name.len() });
    }
    if name == "." || name == ".." {
        return Err(NotAName::Relative);
    }
    if let Some(character) = name.chars().find(|c| !permitted(*c)) {
        return Err(NotAName::Forbidden { character });
    }
    Ok(name)
}

/// One character of a name.
const fn permitted(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
}

#[cfg(test)]
mod tests {
    use super::{checked, NotAName, LONGEST};

    #[test]
    fn an_ordinary_name_is_a_name() {
        for name in ["sales", "regional_2024", "cube-1", "a", "v1.2"] {
            assert!(checked(name).is_ok(), "{name} is an ordinary name");
        }
    }

    /// The `SEC-06` statements, by name. Each of these reached a path built with `join`.
    #[test]
    fn nothing_that_traverses_is_a_name() {
        for name in [
            "..",
            ".",
            "../_cubes/regional",
            "/var/tmp/x",
            "a/b",
            "a\\b",
            "..\\..\\x",
            "\0",
            "a\0b",
            "C:x",
        ] {
            assert!(
                checked(name).is_err(),
                "{name:?} must not become part of a path"
            );
        }
    }

    /// A name is refused with the rule it broke, because a refusal that says only "no" is one
    /// an operator answers by trying variations until something works.
    #[test]
    fn a_refusal_says_which_rule() {
        assert_eq!(checked(""), Err(NotAName::Empty));
        assert_eq!(checked(".."), Err(NotAName::Relative));
        assert_eq!(checked("a/b"), Err(NotAName::Forbidden { character: '/' }));
        let long = "a".repeat(LONGEST + 1);
        assert_eq!(
            checked(&long),
            Err(NotAName::TooLong { was: LONGEST + 1 })
        );
    }

    /// A dot is permitted inside a name and is not permitted to be the whole of one, which is
    /// the distinction that lets `v1.2` through and stops `..`.
    #[test]
    fn a_dot_inside_a_name_is_not_a_traversal() {
        assert!(checked("v1.2").is_ok());
        assert!(checked("a..b").is_ok());
        assert!(checked("...").is_ok());
        assert!(checked("..").is_err());
    }

    /// Non-ASCII letters are refused rather than accepted-and-normalised. Two names that differ
    /// only in normalisation form are one file on macOS and two on Linux, which shows up as a
    /// document silently overwritten rather than as an error.
    #[test]
    fn a_name_is_ascii() {
        assert!(checked("café").is_err());
        assert!(checked("отчёт").is_err());
    }

    /// Every message ends up in front of somebody, so none of them may be empty.
    #[test]
    fn every_refusal_can_be_read() {
        for refusal in [
            NotAName::Empty,
            NotAName::TooLong { was: 500 },
            NotAName::Forbidden { character: '/' },
            NotAName::Relative,
        ] {
            assert!(!refusal.to_string().is_empty(), "{refusal:?} says nothing");
        }
    }
}
