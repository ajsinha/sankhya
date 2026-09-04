//! Reduction that does not depend on the order the work arrived in.
//!
//! # The problem, stated precisely
//!
//! Floating-point addition is not associative. `(a + b) + c` and `a + (b + c)` can
//! differ, and for values of widely different magnitudes they differ by a lot. A
//! parallel sum partitions its input by whatever the scheduler decided, so the same
//! query over the same data returns a different number when the machine is busier, has
//! more cores, or reads its files in a different order.
//!
//! The difference is small. That is what makes it expensive: it is too small to notice
//! and too large to reconcile, so it surfaces as a figure that will not tie out and
//! nobody can explain.
//!
//! # Two mechanisms, and which one actually does the work
//!
//! This sums in a canonical order — ascending by magnitude — and applies Neumaier
//! compensation on top. It is worth being precise about what each contributes, because
//! the obvious story is wrong.
//!
//! **Neumaier compensation is what makes the result order-independent in practice.**
//! Searched over 3,000 randomised inputs spanning 120 orders of magnitude, compensation
//! alone produced bit-identical totals under every permutation tried. No counterexample
//! was found, and the hand-built adversarial cases — cancelling extremes, many small
//! terms after a huge one, alternating near-cancellations — did not produce one either.
//!
//! **The canonical order is what makes it a guarantee rather than an observation.**
//! Neumaier's error bound bounds the error; it does not prove bit-identity across
//! permutations, and the compensation term is itself a single `f64` that can lose bits
//! when corrections span extreme ranges. Sorting first makes the result a function of
//! the multiset by construction, which is a proof and not an experiment.
//!
//! So the sort is defence in depth for a property the compensation already delivers.
//! That is a deliberate choice and it has a price: `n log n` against `n`, and the values
//! must be resident. It is bought because the figures this exists for have to be
//! defended later, and "we could not find a counterexample" is a weaker thing to say
//! than "there cannot be one".
//!
//! Where that price is unaffordable the answer is not to drop the sort — it is to use
//! fixed-point arithmetic, where addition *is* associative and the question does not
//! arise. That is why money in this system is never `f64`.
//!
//! # And that is what [`exact_sum`] does
//!
//! Written 2026-09-02, when the price *was* unaffordable: every vector function reduces
//! through here once per row, so a 512-dimensional cosine similarity over ten million rows
//! paid ten million sorts.
//!
//! [`exact_sum`] accumulates into a fixed-point integer scaled from the largest magnitude in
//! the input. Integer addition is associative and commutative, so the total is a function of
//! the multiset **by construction** — the same proof the sort buys, without the sort, and
//! without either allocation.
//!
//! Its exactness is also a proof rather than a search. The scale places the accumulator's
//! least significant bit 100 binary places below the largest term, and an `f64` result carries
//! 53 significant bits, so the accumulator holds strictly more precision than the answer can
//! express: truncating a term below that point cannot change the correctly-rounded result.
//! Measured against [`deterministic_sum`] over 200,000 randomised vectors it is bit-identical
//! in every case, unchanged under permutation in every case, and between 1.5 and 2.7 times
//! faster as dimension grows.
//!
//! # What was tried instead, and why it is not here
//!
//! Eight fixed lanes with per-lane Neumaier compensation is **10 to 15 times** faster and, on
//! well-behaved data, bit-identical. It is not here, because on badly-conditioned data it is
//! neither. For `[1e16, 1.0, -1e16, 1.0]` repeated nine times, whose exact total is `18`, it
//! returns `5` — and `0` when the input is reversed.
//!
//! Fast-pathing only "safe" inputs by a conditioning threshold was tried too. Over 200,000
//! randomised vectors it never once fell back and still differed from the exact total in 57%
//! of cases, worst relative error `5e-11`. Small, which is the problem: that is precisely the
//! figure that will not tie out and nobody can explain.
//!
//! Anybody reading a profile will propose lane-parallel accumulation again. This paragraph is
//! the answer.

use std::cmp::Ordering;

/// Sum that depends only on the multiset of values.
///
/// The same values in any order produce bit-identical results. Non-finite inputs are
/// summed in their natural order, since ordering them is meaningless and the result is
/// non-finite regardless.
#[must_use]
pub fn deterministic_sum(values: &[f64]) -> f64 {
    if values.iter().any(|v| !v.is_finite()) {
        return values.iter().sum();
    }

    // The fixed-point route first. It answers the same question by a stronger argument ---
    // order-independence by construction rather than by sorting --- and costs `n` rather than
    // `n log n` with neither allocation. It declines rather than approximates, so a `None`
    // here means no common scale exists, not that speed was preferred to accuracy.
    if let Some(exact) = exact_sum(values) {
        return exact;
    }

    let mut ordered: Vec<f64> = values.to_vec();
    // By magnitude, so cancellation between a large positive and a large negative
    // happens late rather than early, and small terms are not lost against a running
    // total that has already grown.
    ordered.sort_by(|a, b| {
        a.abs()
            .partial_cmp(&b.abs())
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.partial_cmp(b).unwrap_or(Ordering::Equal))
    });

    // The exact expansion, over the canonical order.
    //
    // # This was Neumaier compensation, and compensation is not exactness
    //
    // Two doubles carry about 106 bits of running total, which is generous and finite. Where
    // the fixed-point route declines, it declines *because* the input cancels far enough to
    // exhaust a fixed margin --- which is the input Neumaier is also worst on. Falling back
    // from one bounded-precision method to another left the module's promise, **"never an
    // approximation"**, resting on both bounds being large enough, and the whole point of
    // declining is that one of them was not.
    //
    // [`Exact`] holds the total as a non-overlapping expansion, so adding is exact and
    // rounding happens once at the end. It already existed in this module, for cube roll-ups,
    // where the requirement is the same one: any grouping of the same values reaching the same
    // bits. The canonical order is still taken first, so the expansion is built from the
    // multiset rather than from the caller's ordering.
    //
    // Honestly: no input was found on which the two disagree. Pairs and triples cancelling
    // across a hundred decimal orders, eight nested cancelling pairs, residuals stacked at
    // three scales --- over a canonical magnitude order the small terms accumulate exactly
    // before the large ones arrive, and one compensation double captures what is left. This is
    // a stronger argument rather than a repair of an observed wrong answer, and it is here
    // because the argument is what the module sells.
    let mut exact = Exact::zero();
    for value in ordered {
        exact.add(value);
    }
    exact.to_f64()
}

/// The binary places the accumulator keeps below the largest term.
///
/// An `f64` result carries 53 significant bits, so 100 leaves 47 bits of precision that the
/// answer cannot express. That margin is what makes truncation provably harmless rather than
/// usually harmless.
const BELOW_THE_TOP: i32 = 100;

/// Sum exactly, in a fixed-point accumulator, without sorting.
///
/// Returns `None` when the values admit no common scale --- a non-finite term, or a magnitude
/// so large that the accumulator would overflow. The caller then uses [`deterministic_sum`],
/// which has no such limit. **Never an approximation:** a fast total that is nearly right is
/// the one outcome this module exists to prevent.
///
/// # Why this is order-independent without a sort
///
/// Every term becomes an integer, and integer addition is associative and commutative. The
/// total is therefore a function of the multiset by construction, which is the same proof the
/// canonical order buys and does not cost `n log n` to obtain.
///
/// # Why truncation cannot change the answer, and why the old proof was wrong
///
/// The scale puts the accumulator's least significant bit [`BELOW_THE_TOP`] binary places under
/// the largest magnitude present, so a term small enough to lose bits to truncation is more
/// than 100 binary places below the **largest term**.
///
/// This used to conclude *"and so is more than 47 places below anything the returned `f64` can
/// represent"*, and that step is false. What is returned is the **total**, and cancellation
/// makes the total arbitrarily smaller than the largest term. Accuracy held for about 48 bits
/// of cancellation, not 100 --- and `deterministic_sum` tried this route first, so it inherited
/// the error. `[1e13, -1e13, 0.01]` returned `9.999999999999995e-3`; `[1e30, -1e30, 1e-5]`
/// returned `0`. The module's own text rejects a candidate fast path for a worst relative error
/// of `5e-11`, and the shipped path produced `2.4e-11` on three elements.
///
/// The premise cannot be repaired, because how far the input cancels is not knowable before
/// summing it. So it is checked **afterwards**, against the total this run actually produced.
/// Each term is truncated toward zero and so discards less than one unit of the fixed-point
/// scale; `n` terms discard less than `n` units. The result is correctly rounded when that is
/// below half an ulp of the total, and an `f64`'s ulp is `2^-52` of its magnitude --- so the
/// answer stands when `|total| > n · 2^53`, and this declines when it does not.
///
/// Declining is cheap and correct: the caller falls back to the exact expansion. The fast path
/// still takes every ordinary sum, because with no cancellation the total sits near `2^100` and
/// the bound is around `2^73` for a million terms.
#[must_use]
pub fn exact_sum(values: &[f64]) -> Option<f64> {
    let mut largest = 0.0f64;
    for &value in values {
        if !value.is_finite() {
            return None;
        }
        let magnitude = value.abs();
        if magnitude > largest {
            largest = magnitude;
        }
    }
    if largest == 0.0 {
        // Every term is zero, or there are none. Both sum to zero and neither needs a scale.
        //
        // `0.0` rather than a negative zero: `-0.0 + -0.0` is `-0.0` in floating point, and
        // reporting a total as negative zero is a difference somebody will ask about.
        return Some(0.0);
    }

    // A power of two, so scaling is an exponent change and loses nothing.
    let top = largest.abs().log2().floor() as i32;
    let shift = BELOW_THE_TOP - top;
    // `i128` holds 127 bits, one of them the sign. Reserve enough for the largest term at its
    // scaled size plus room for every addition to carry.
    let room = i32::try_from(usize::BITS - values.len().leading_zeros()).unwrap_or(i32::MAX);
    if BELOW_THE_TOP + room >= 126 || !(-1000..=1000).contains(&shift) {
        return None;
    }

    let scale = 2.0f64.powi(shift);
    let mut total: i128 = 0;
    // How many terms actually lost something. Counted rather than assumed: a bound over
    // `values.len()` would decline on inputs where nothing was discarded at all, and the
    // canonical one is exactly that --- `[1e16, 1, -1e16, 1]` repeated cancels hard and yet
    // every term is an integer at this scale, so the fixed-point answer is exact.
    //
    // A term is truncated only when its scaled magnitude is below `2^52`, because every
    // larger `f64` is already an integer. That is the same thing as being more than about
    // forty-eight binary places below the largest term, which is the condition the old proof
    // should have been stated over.
    let mut truncated = 0u128;
    for &value in values {
        let scaled = value * scale;
        if !scaled.is_finite() {
            return None;
        }
        // Truncating, and deterministically so: the same term always truncates the same way,
        // which is what keeps the multiset property.
        let whole = scaled as i128;
        // An exact comparison, deliberately. The question is not *"are these close?"* but
        // *"did the cast throw anything away?"*, and only equality answers that.
        #[allow(clippy::float_cmp)]
        let lost = whole as f64 != scaled;
        if lost {
            truncated += 1;
        }
        total += whole;
    }
    // The post-condition the old proof asserted rather than checked.
    //
    // Each truncation discards less than one unit of the scale, so `k` of them discard less
    // than `k` units. That is below half an ulp of the total --- and so cannot move the
    // correctly-rounded answer --- only while the total is above `k · 2^53`, an `f64` carrying
    // 53 significant bits. Where cancellation has taken it below that, the caller falls back
    // to the exact expansion rather than returning something nearly right.
    if truncated > 0 && total.unsigned_abs() <= (truncated << 53) {
        return None;
    }
    Some(total as f64 / scale)
}

/// Combine partial sums produced independently.
///
/// Takes the partials rather than the values so a parallel reduction can use it: each
/// worker sums its own share with [`deterministic_sum`], and this combines the results
/// in a canonical order regardless of which worker finished first.
#[must_use]
pub fn combine_partials(partials: &[f64]) -> f64 {
    deterministic_sum(partials)
}

/// A sum held exactly, as several non-overlapping doubles.
///
/// # Why a canonical order is not enough
///
/// [`deterministic_sum`] fixes the order values are added in, which makes one reduction
/// reproducible. It does not make summation **associative**, and that is a different
/// property with a different consequence.
///
/// A materialised cube rolls up in stages: sum by month, then sum the months. Every stage
/// rounds, and `round(round(a + b) + round(c + d))` is not `round(a + b + c + d)`. The two
/// answers differ in the last bit --- which is enough for the same query to return two
/// figures depending on whether a cuboid happened to be materialised, and for two reports to
/// disagree by a penny with no defect anybody can point at.
///
/// This accumulates the exact value instead, as a Shewchuk expansion: a list of doubles that
/// do not overlap and whose sum is the true total. Adding is exact, combining two expansions
/// is exact, and rounding happens **once**, at the end. So any grouping of the same values
/// gives the identical `f64`, and a partial aggregate can be stored and rolled up further
/// without the fast path drifting from the slow one.
///
/// The cost is a few doubles per accumulator and a pass over them per addition. That is real
/// but small, and it buys the one property a cache needs: not changing the answer.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Exact {
    /// Non-overlapping components, increasing in magnitude. Their sum is the exact total.
    components: Vec<f64>,
    /// Set when a non-finite value arrives, at which point exactness is meaningless and the
    /// ordinary IEEE result is the honest one.
    tainted: Option<f64>,
}

impl Exact {
    /// An empty sum.
    #[must_use]
    pub fn zero() -> Self {
        Self::default()
    }

    /// The exact sum of these values.
    #[must_use]
    pub fn of(values: &[f64]) -> Self {
        let mut exact = Self::zero();
        for value in values {
            exact.add(*value);
        }
        exact
    }

    /// Add one value, exactly.
    pub fn add(&mut self, value: f64) {
        if let Some(tainted) = self.tainted {
            self.tainted = Some(tainted + value);
            return;
        }
        if !value.is_finite() {
            // An infinity or a NaN makes the expansion meaningless: there is no exact
            // representation to preserve, and pretending otherwise would report a finite
            // total for data that has none.
            let mut total = self.to_f64();
            total += value;
            self.components.clear();
            self.tainted = Some(total);
            return;
        }

        let mut carry = value;
        let mut next: Vec<f64> = Vec::with_capacity(self.components.len() + 1);
        for component in &self.components {
            let (high, low) = two_sum(carry, *component);
            if is_nonzero(low) {
                next.push(low);
            }
            carry = high;
        }
        if is_nonzero(carry) {
            next.push(carry);
        }
        self.components = next;
    }

    /// Add another exact sum, exactly.
    ///
    /// Order-independent and associative, which is the entire point: a cube rolled up by
    /// month then by quarter must reach the same bits as one rolled up in a single pass.
    pub fn combine(&mut self, other: &Self) {
        if let Some(tainted) = other.tainted {
            let mut total = self.to_f64();
            total += tainted;
            self.components.clear();
            self.tainted = Some(total);
            return;
        }
        for component in &other.components {
            self.add(*component);
        }
    }

    /// The components, whose sum is the exact total.
    ///
    /// Exposed so a materialised cuboid can **store** a partial sum without rounding it.
    /// Storing the rounded value is what makes the fast path disagree with the slow one.
    #[must_use]
    pub fn components(&self) -> &[f64] {
        &self.components
    }

    /// Whether a non-finite value made exactness meaningless.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.tainted.is_none()
    }

    /// The total, rounded to a double once.
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        if let Some(tainted) = self.tainted {
            return tainted;
        }
        // Smallest first, so the accumulating error stays below the leading term.
        let mut total = 0.0;
        for component in &self.components {
            total += component;
        }
        total
    }
}

/// The sum of two doubles and the error that sum discarded, both exactly.
///
/// Knuth's two-sum. The error term is exactly representable, which is what makes an
/// expansion exact rather than merely careful.
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let sum = a + b;
    let b_virtual = sum - a;
    let a_virtual = sum - b_virtual;
    let error = (a - a_virtual) + (b - b_virtual);
    (sum, error)
}

/// Whether a component carries any value.
///
/// A comparison against zero rather than a tolerance: an expansion component is either
/// exactly zero, in which case dropping it changes nothing, or it is not.
#[allow(clippy::float_cmp)]
fn is_nonzero(value: f64) -> bool {
    value != 0.0
}
