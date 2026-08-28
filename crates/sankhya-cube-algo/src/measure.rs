//! Measures, and the rule that decides whether a number means anything.
//!
//! # A measure with no declared aggregation rule is refused
//!
//! Not defaulted to summation. This is the single most consequential decision in the cube
//! model, because **the default is wrong for an entire class of measures and wrong
//! invisibly**.
//!
//! A closing balance summed across twelve months gives the sum of twelve month-end balances.
//! That is not a quantity anybody wanted, it is not obviously absurd, and it looks exactly
//! like a number that means something. A rate averaged across regions without weighting is
//! wrong in a way no test catches, because nothing is missing and nothing is null.
//!
//! So the rule is declared per measure **and per dimension**, and a definition without one
//! does not load.
//!
//! # Per dimension, and that is the part people skip
//!
//! "This measure is semi-additive" is not a usable statement. Semi-additive *over what?* A
//! balance sums across accounts and takes the last value across time; an inventory count
//! sums across warehouses and takes the last across time; a headcount sums across
//! departments and takes the last across time --- and each of those is a different dimension
//! in a different cube.
//!
//! At query time the planner asks one question: **may this measure be summed along *this*
//! axis?** A per-measure answer cannot answer it.

use std::fmt;

/// How a measure combines along one dimension.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// Values add.
    Sum,
    /// The last value along this dimension, in its natural order.
    ///
    /// The rule for a balance over time: a bank account's December balance is not the sum of
    /// its twelve month-end balances.
    Last,
    /// The first value along this dimension.
    First,
    /// The largest, or the smallest.
    Max,
    /// The smallest.
    Min,
    /// The arithmetic mean, which is **not** safe to compose.
    ///
    /// An average of averages is not an average unless every group is the same size, and
    /// groups are never the same size. Declared so it can be refused where it would be
    /// composed rather than silently produce a plausible figure --- see
    /// [`Rule::composes`].
    Mean,
    /// Cannot be derived from its children at all.
    ///
    /// A ratio, a distinct count, a percentile. There is no operation over the parts that
    /// yields the whole, so a query for the whole must go to the base data.
    None,
}

impl Rule {
    /// Whether an aggregate along this dimension can be built from partial aggregates.
    ///
    /// The property that decides whether a materialised cuboid may be rolled up further, and
    /// the reason `Mean` is excluded despite being a perfectly good aggregate: it is not
    /// *associative* over groups. `Sum`, `Min` and `Max` are; `Last` and `First` are, given
    /// an order; `Mean` and `None` are not.
    #[must_use]
    pub const fn composes(self) -> bool {
        matches!(self, Self::Sum | Self::Last | Self::First | Self::Max | Self::Min)
    }

    /// Its name in a message.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Last => "last",
            Self::First => "first",
            Self::Max => "max",
            Self::Min => "min",
            Self::Mean => "mean",
            Self::None => "none",
        }
    }
}

/// A measure's rule along one named dimension.
/// # Why these are owned rather than `&'static str`
///
/// They were `&'static str` and `&'static [Along]`, which made a measure a **compile-time**
/// construct: a definition could name only measures a Rust source file had already spelled
/// out. That is the reason a cube had to be registered against a session by the embedding
/// application and could not be persisted, loaded, or authored by anybody who was not
/// recompiling the server.
///
/// The cost of owning them is an allocation per declared measure, once, when a definition is
/// built. The cost of not owning them was that cubes could not be data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Along {
    /// The dimension.
    pub dimension: String,
    /// How it combines there.
    pub rule: Rule,
}

/// A declared measure.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Measure {
    /// What it is called.
    pub name: String,
    /// Its rule along each dimension of the cube.
    ///
    /// Every dimension, with no default. A missing entry is a definition error rather than
    /// an implicit `Sum`, which is the whole point.
    pub rules: Vec<Along>,
}

impl Along {
    /// How a measure combines along one dimension.
    pub fn new(dimension: impl Into<String>, rule: Rule) -> Self {
        Self { dimension: dimension.into(), rule }
    }
}

impl Measure {
    /// A measure and its rule along each dimension.
    ///
    /// Every dimension, with no default: a missing entry is a definition error rather than
    /// an implicit `Sum`. See [`Measure::rules`].
    pub fn new(name: impl Into<String>, rules: Vec<Along>) -> Self {
        Self { name: name.into(), rules }
    }
}

impl Measure {
    /// The rule along a dimension, if one was declared.
    #[must_use]
    pub fn rule(&self, dimension: &str) -> Option<Rule> {
        self.rules
            .iter()
            .find(|along| along.dimension == dimension)
            .map(|along| along.rule)
    }

    /// Whether every dimension of the cube has a rule.
    ///
    /// # Errors
    ///
    /// Names every dimension without one, not the first. An author fixing them one at a time
    /// learns about the next only after another load.
    pub fn covers(&self, dimensions: &[&str]) -> Result<(), Undeclared> {
        let missing: Vec<String> = dimensions
            .iter()
            .filter(|dimension| self.rule(dimension).is_none())
            .map(|dimension| (*dimension).to_string())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Undeclared {
                measure: self.name.to_string(),
                dimensions: missing,
            })
        }
    }

    /// Whether this measure is additive along every dimension.
    #[must_use]
    pub fn additive_everywhere(&self) -> bool {
        !self.rules.is_empty() && self.rules.iter().all(|along| along.rule == Rule::Sum)
    }

    /// The dimensions this measure is **not** additive along.
    ///
    /// What a planner needs to know before rolling anything up.
    #[must_use]
    pub fn not_additive_along(&self) -> Vec<&str> {
        self.rules
            .iter()
            .filter(|along| along.rule != Rule::Sum)
            .map(|along| along.dimension.as_str())
            .collect()
    }
}

/// A measure that does not say how it combines somewhere.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Undeclared {
    /// Which measure.
    pub measure: String,
    /// Which dimensions it says nothing about.
    pub dimensions: Vec<String>,
}

impl fmt::Display for Undeclared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the measure `{}` declares no aggregation rule along {}. It is refused rather \
             than defaulted to summation, because the default is wrong for an entire class \
             of measures and wrong invisibly: a closing balance summed across twelve months \
             gives the sum of twelve month-end balances, which is not a quantity anybody \
             wanted and looks exactly like one that is",
            self.measure,
            self.dimensions.join(", ")
        )
    }
}

impl std::error::Error for Undeclared {}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
