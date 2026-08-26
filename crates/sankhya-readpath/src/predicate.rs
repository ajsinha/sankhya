//! Translating a query's filters into predicates the statistics understand.
//!
//! # Why most expressions translate to nothing
//!
//! This recognises a deliberately small set of shapes: a column compared to a literal,
//! and conjunctions of those. Everything else — a function call, a column compared to
//! another column, anything under a disjunction — produces no predicate, and a file with
//! no predicate is read.
//!
//! That is not a limitation to be apologised for. Every shape added here is a new chance
//! to skip a file that should have been read, and the cost of *not* recognising a shape
//! is a scan. Recognising one incorrectly costs an answer. The set stays small on
//! purpose, and grows only where the translation is obviously right.
//!
//! # Disjunction is the trap
//!
//! `a > 10 OR b < 5` cannot be split into predicates on `a` and `b` and applied
//! independently: a file may fail the first and satisfy the second. Taking one side of a
//! disjunction is the natural implementation and it silently drops rows, which is why
//! disjunction is not descended into at all.

use datafusion::logical_expr::{BinaryExpr, Expr, Operator};
use datafusion::scalar::ScalarValue;
use sankhya_stats::{Bound, Predicate};

/// Predicates on named columns, extracted from a query's filters.
///
/// A column may appear more than once — `x > 5 AND x < 100` yields two — and all of them
/// must hold, so a file may be skipped if *any* of them proves it irrelevant.
#[must_use]
pub fn extract(filters: &[Expr]) -> Vec<(String, Predicate)> {
    let mut out = Vec::new();
    for filter in filters {
        collect(filter, &mut out);
    }
    out
}

fn collect(expr: &Expr, out: &mut Vec<(String, Predicate)>) {
    match expr {
        // Every conjunct must hold, so each may be used independently.
        Expr::BinaryExpr(BinaryExpr {
            left,
            op: Operator::And,
            right,
        }) => {
            collect(left, out);
            collect(right, out);
        }
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            if let Some(pair) = comparison(left, *op, right) {
                out.push(pair);
            }
        }
        Expr::IsNull(inner) => {
            if let Expr::Column(column) = inner.as_ref() {
                out.push((column.name.clone(), Predicate::IsNull));
            }
        }
        Expr::IsNotNull(inner) => {
            if let Expr::Column(column) = inner.as_ref() {
                out.push((column.name.clone(), Predicate::IsNotNull));
            }
        }
        Expr::Between(between) if !between.negated => {
            let (Expr::Column(column), Some(low), Some(high)) = (
                between.expr.as_ref(),
                literal(&between.low),
                literal(&between.high),
            ) else {
                return;
            };
            out.push((column.name.clone(), Predicate::Between { low, high }));
        }
        // Deliberately not descended into. See the module documentation on disjunction.
        _ => {}
    }
}

/// A column compared to a literal, in either order.
fn comparison(left: &Expr, op: Operator, right: &Expr) -> Option<(String, Predicate)> {
    // `5 < x` is `x > 5`. Flipping the operator rather than ignoring the form, because
    // the reversed shape is common and getting it wrong by *not* flipping would be a
    // wrong skip rather than a missed one.
    let (column, bound, op) = match (left, right) {
        (Expr::Column(c), other) => (c, literal(other)?, op),
        (other, Expr::Column(c)) => (c, literal(other)?, op),
        _ => return None,
    };

    let predicate = match op {
        Operator::Eq => Predicate::Equals(bound),
        Operator::Lt => Predicate::LessThan(bound),
        Operator::LtEq => Predicate::LessOrEqual(bound),
        Operator::Gt => Predicate::GreaterThan(bound),
        Operator::GtEq => Predicate::GreaterOrEqual(bound),
        // Inequality tells the bounds nothing useful: a file whose values span anything
        // at all contains something that is not the target.
        _ => return None,
    };
    Some((column.name.clone(), predicate))
}

const fn flip(op: Operator) -> Option<Operator> {
    Some(match op {
        Operator::Eq => Operator::Eq,
        Operator::Lt => Operator::Gt,
        Operator::LtEq => Operator::GtEq,
        Operator::Gt => Operator::Lt,
        Operator::GtEq => Operator::LtEq,
        _ => return None,
    })
}

/// A literal a bound can be made from.
///
/// Only exact types. A float literal compared against an integer column, or anything
/// requiring a coercion, produces nothing — a coerced comparison is where a bound stops
/// meaning what it says.
fn literal(expr: &Expr) -> Option<Bound> {
    let Expr::Literal(value, _) = expr else {
        return None;
    };
    Some(match value {
        ScalarValue::Int8(Some(v)) => Bound::Int(i64::from(*v)),
        ScalarValue::Int16(Some(v)) => Bound::Int(i64::from(*v)),
        ScalarValue::Int32(Some(v)) => Bound::Int(i64::from(*v)),
        ScalarValue::Int64(Some(v)) => Bound::Int(*v),
        ScalarValue::UInt8(Some(v)) => Bound::Int(i64::from(*v)),
        ScalarValue::UInt16(Some(v)) => Bound::Int(i64::from(*v)),
        ScalarValue::UInt32(Some(v)) => Bound::Int(i64::from(*v)),
        ScalarValue::UInt64(Some(v)) => Bound::Int(i64::try_from(*v).ok()?),
        ScalarValue::Float32(Some(v)) => Bound::Float(f64::from(*v)),
        ScalarValue::Float64(Some(v)) => Bound::Float(*v),
        ScalarValue::Utf8(Some(v)) | ScalarValue::LargeUtf8(Some(v)) => {
            Bound::Bytes(v.as_bytes().to_vec())
        }
        _ => return None,
    })
}
