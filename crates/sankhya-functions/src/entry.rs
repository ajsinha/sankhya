//! What the catalogue says about one function.
//!
//! # Why the catalogue is data rather than documentation
//!
//! Until this existed, *"which functions does this server have"* was answerable only by reading
//! Rust. Two things followed, and both were real:
//!
//! - **No binding could offer them.** The Python SDK had a method for cloning and none for any
//!   of a hundred and seventeen kernels, because a binding cannot generate what it cannot
//!   enumerate. `ADR-0020` Decision 5 calls a function undelivered until a binding can call it,
//!   so by this project's own standard they were half-shipped.
//! - **No router could classify a statement.** `ADR-0020` Decision 2 says a statement calling a
//!   built-in routes to the analytical tier, and deciding that needs a list of what the
//!   built-ins *are*.
//!
//! So the catalogue is a value. The SQL surface serves it, a binding reads it, and the router
//! matches against it --- one list, three consumers, and no second copy to disagree.

/// What a function takes.
///
/// Named rather than described as a type signature, because the shapes a caller has to
/// distinguish are few and a type signature would have to be parsed to be useful.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Takes {
    /// Plain numbers.
    Numbers,
    /// One array of doubles.
    Series,
    /// One flat, row-major square matrix.
    Matrix,
    /// One array and one number.
    SeriesAndNumber,
    /// Several arrays, or arrays and numbers.
    Several,
}

impl Takes {
    /// The word the catalogue reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Numbers => "numbers",
            Self::Series => "a series",
            Self::Matrix => "a matrix",
            Self::SeriesAndNumber => "a series and a number",
            Self::Several => "several arrays",
        }
    }
}

/// What a function gives back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gives {
    /// One number.
    Number,
    /// An array of doubles.
    Series,
}

impl Gives {
    /// The word the catalogue reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Number => "a number",
            Self::Series => "a series",
        }
    }
}

/// One function, as the catalogue describes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The name a statement calls it by.
    pub name: &'static str,
    /// Which family it belongs to, for a client offering a picker.
    pub category: &'static str,
    /// How many arguments it takes.
    pub arity: usize,
    /// What those arguments are.
    pub takes: Takes,
    /// What comes back.
    pub gives: Gives,
    /// One line, for somebody choosing between two names.
    pub about: &'static str,
}

impl Entry {
    /// Declare one.
    #[must_use]
    pub const fn new(
        name: &'static str,
        category: &'static str,
        arity: usize,
        takes: Takes,
        gives: Gives,
        about: &'static str,
    ) -> Self {
        Self { name, category, arity, takes, gives, about }
    }
}
