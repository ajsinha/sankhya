//! A small expression language, so most of a pack needs no Rust.
//!
//! # Why this exists
//!
//! The substantial majority of a real domain pack is not novel computation. It is *naming*
//! --- giving an organisation's vocabulary to a threshold, a comparison, a ratio --- and
//! composition of things that already exist. Requiring a compiled crate for that means a
//! rebuild and a redeploy to change a number, which in practice means the number does not
//! get changed.
//!
//! So this tier expresses that majority declaratively, and building it well is what keeps
//! the compiled tier for the cases that genuinely need it.
//!
//! # What it deliberately cannot do
//!
//! No loops, no recursion, no I/O, no allocation of unbounded size. Every expression
//! terminates in time proportional to its own text, which is fixed when the bundle is
//! loaded. A declarative function therefore **cannot** be the one that hangs a query ---
//! the sandbox exists for the compiled tier, and this tier is safe by construction rather
//! than by supervision.
//!
//! That is a real restriction and it is the point. An expression language with loops is a
//! programming language, and a programming language loaded from a configuration file is a
//! remote code execution feature with extra steps.

use sankhya_ext::value::{LogicalType, Value};
use std::collections::BTreeMap;

/// A parsed expression.
#[derive(Clone, PartialEq, Debug)]
pub enum Expr {
    /// A constant.
    Literal(Value),
    /// One of the function's arguments, by name.
    Argument(String),
    /// An operator applied to two operands.
    Binary {
        /// What to do.
        op: BinaryOp,
        /// The left operand.
        left: Box<Expr>,
        /// The right operand.
        right: Box<Expr>,
    },
    /// An operator applied to one.
    Unary {
        /// What to do.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// A choice between two values.
    IfElse {
        /// The condition.
        condition: Box<Expr>,
        /// The value when true.
        then: Box<Expr>,
        /// The value when false.
        otherwise: Box<Expr>,
    },
}

/// A two-operand operator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Subtract,
    /// Multiplication.
    Multiply,
    /// Division.
    Divide,
    /// Equality.
    Equal,
    /// Inequality.
    NotEqual,
    /// Strictly less.
    Less,
    /// Less or equal.
    LessOrEqual,
    /// Strictly greater.
    Greater,
    /// Greater or equal.
    GreaterOrEqual,
    /// Conjunction.
    And,
    /// Disjunction.
    Or,
}

/// A one-operand operator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnaryOp {
    /// Logical negation.
    Not,
    /// Arithmetic negation.
    Negate,
}

/// Why an expression could not be evaluated.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EvalError {
    /// What went wrong.
    pub detail: String,
}

impl EvalError {
    fn of(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for EvalError {}

impl Expr {
    /// Evaluate against a set of named arguments.
    ///
    /// # Null propagation
    ///
    /// Any operand being null makes the result null, except under `and`/`or`, which
    /// short-circuit the way SQL does: `false and null` is false, because the answer is
    /// already known. Getting this wrong turns a missing value into a `false` and a filter
    /// silently excludes rows it should have kept.
    pub fn evaluate(&self, arguments: &BTreeMap<String, Value>) -> Result<Value, EvalError> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Argument(name) => arguments
                .get(name)
                .cloned()
                .ok_or_else(|| EvalError::of(format!("no argument named '{name}'"))),
            Self::Unary { op, operand } => {
                let value = operand.evaluate(arguments)?;
                if value.is_null() {
                    return Ok(Value::Null);
                }
                match op {
                    UnaryOp::Not => match value {
                        Value::Boolean(b) => Ok(Value::Boolean(!b)),
                        other => Err(EvalError::of(format!(
                            "'not' needs a boolean, got {other:?}"
                        ))),
                    },
                    UnaryOp::Negate => match value {
                        Value::Integer(n) => Ok(Value::Integer(n.saturating_neg())),
                        Value::Real(x) => Ok(Value::Real(-x)),
                        other => Err(EvalError::of(format!("cannot negate {other:?}"))),
                    },
                }
            }
            Self::IfElse {
                condition,
                then,
                otherwise,
            } => match condition.evaluate(arguments)? {
                Value::Boolean(true) => then.evaluate(arguments),
                Value::Boolean(false) => otherwise.evaluate(arguments),
                // A null condition takes neither branch. Treating it as false would make
                // "unknown" mean "no", which is a different claim.
                Value::Null => Ok(Value::Null),
                other => Err(EvalError::of(format!(
                    "a condition must be boolean, got {other:?}"
                ))),
            },
            Self::Binary { op, left, right } => Self::binary(*op, left, right, arguments),
        }
    }

    fn binary(
        op: BinaryOp,
        left: &Self,
        right: &Self,
        arguments: &BTreeMap<String, Value>,
    ) -> Result<Value, EvalError> {
        // Short-circuit before evaluating the right side, so `x <> 0 and 100 / x > 1` does
        // not divide by zero, and so a null on one side can still give a definite answer.
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let first = left.evaluate(arguments)?;
            match (op, &first) {
                (BinaryOp::And, Value::Boolean(false)) => return Ok(Value::Boolean(false)),
                (BinaryOp::Or, Value::Boolean(true)) => return Ok(Value::Boolean(true)),
                _ => {}
            }
            let second = right.evaluate(arguments)?;
            return match (&first, &second) {
                (Value::Boolean(a), Value::Boolean(b)) => Ok(Value::Boolean(match op {
                    BinaryOp::And => *a && *b,
                    _ => *a || *b,
                })),
                (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                _ => Err(EvalError::of("'and' and 'or' need booleans")),
            };
        }

        let a = left.evaluate(arguments)?;
        let b = right.evaluate(arguments)?;
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }

        match op {
            BinaryOp::Equal => Ok(Value::Boolean(
                compare(&a, &b)? == std::cmp::Ordering::Equal,
            )),
            BinaryOp::NotEqual => Ok(Value::Boolean(
                compare(&a, &b)? != std::cmp::Ordering::Equal,
            )),
            BinaryOp::Less => Ok(Value::Boolean(compare(&a, &b)? == std::cmp::Ordering::Less)),
            BinaryOp::LessOrEqual => Ok(Value::Boolean(
                compare(&a, &b)? != std::cmp::Ordering::Greater,
            )),
            BinaryOp::Greater => Ok(Value::Boolean(
                compare(&a, &b)? == std::cmp::Ordering::Greater,
            )),
            BinaryOp::GreaterOrEqual => {
                Ok(Value::Boolean(compare(&a, &b)? != std::cmp::Ordering::Less))
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                arithmetic(op, &a, &b)
            }
            BinaryOp::And | BinaryOp::Or => unreachable_and_or(),
        }
    }
}

/// The `and`/`or` arms are handled before this point; this keeps the match exhaustive
/// without an `unreachable!`, which the workspace lints forbid.
fn unreachable_and_or() -> Result<Value, EvalError> {
    Err(EvalError::of(
        "'and' and 'or' are short-circuited before this point",
    ))
}

/// Compare two values of compatible type.
fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering, EvalError> {
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => Ok(x.cmp(y)),
        (Value::Instant(x), Value::Instant(y)) => Ok(x.cmp(y)),
        (Value::Text(x), Value::Text(y)) => Ok(x.cmp(y)),
        (Value::Boolean(x), Value::Boolean(y)) => Ok(x.cmp(y)),
        (Value::Bytes(x), Value::Bytes(y)) => Ok(x.cmp(y)),
        _ => {
            // Anything numeric that is not two integers is compared as reals. Two integers
            // are compared exactly above, so this never silently widens a comparison that
            // could have been exact.
            let (Some(x), Some(y)) = (a.as_real(), b.as_real()) else {
                return Err(EvalError::of(format!(
                    "cannot compare {a:?} with {b:?}: they are not the same kind of value"
                )));
            };
            x.partial_cmp(&y)
                .ok_or_else(|| EvalError::of("comparison with a value that is not a number"))
        }
    }
}

/// Add, subtract, multiply or divide.
fn arithmetic(op: BinaryOp, a: &Value, b: &Value) -> Result<Value, EvalError> {
    if let (Value::Integer(x), Value::Integer(y)) = (a, b) {
        // Integers stay integers, and overflow saturates rather than wrapping. A wrapped
        // total is a wrong number that looks ordinary; a saturated one is wrong in a
        // direction that is obvious.
        return Ok(match op {
            BinaryOp::Add => Value::Integer(x.saturating_add(*y)),
            BinaryOp::Subtract => Value::Integer(x.saturating_sub(*y)),
            BinaryOp::Multiply => Value::Integer(x.saturating_mul(*y)),
            BinaryOp::Divide => {
                if *y == 0 {
                    return Err(EvalError::of("division by zero"));
                }
                Value::Integer(x / y)
            }
            _ => return Err(EvalError::of("not an arithmetic operator")),
        });
    }

    let (Some(x), Some(y)) = (a.as_real(), b.as_real()) else {
        return Err(EvalError::of(format!(
            "cannot do arithmetic on {a:?} and {b:?}"
        )));
    };
    Ok(match op {
        BinaryOp::Add => Value::Real(x + y),
        BinaryOp::Subtract => Value::Real(x - y),
        BinaryOp::Multiply => Value::Real(x * y),
        BinaryOp::Divide => {
            if y == 0.0 {
                // Not infinity. An infinite result sorts to the top of every ranked list
                // and reads as an extreme finding rather than a missing denominator.
                return Err(EvalError::of("division by zero"));
            }
            Value::Real(x / y)
        }
        _ => return Err(EvalError::of("not an arithmetic operator")),
    })
}

/// What type an expression produces, given its arguments' types.
///
/// Computed at load rather than at call, so a bundle declaring the wrong return type is
/// refused when it is loaded rather than when a query happens to reach it.
#[must_use]
pub fn result_type(expr: &Expr, arguments: &BTreeMap<String, LogicalType>) -> Option<LogicalType> {
    match expr {
        Expr::Literal(value) => value.logical_type(),
        Expr::Argument(name) => arguments.get(name).cloned(),
        Expr::Unary { op, operand } => match op {
            UnaryOp::Not => Some(LogicalType::Boolean),
            UnaryOp::Negate => result_type(operand, arguments),
        },
        Expr::IfElse {
            then, otherwise, ..
        } => result_type(then, arguments).or_else(|| result_type(otherwise, arguments)),
        Expr::Binary { op, left, right } => match op {
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessOrEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterOrEqual
            | BinaryOp::And
            | BinaryOp::Or => Some(LogicalType::Boolean),
            _ => {
                let a = result_type(left, arguments)?;
                let b = result_type(right, arguments)?;
                // Two integers stay an integer; anything else widens to a real.
                if a == LogicalType::Integer && b == LogicalType::Integer {
                    Some(LogicalType::Integer)
                } else {
                    Some(LogicalType::Real)
                }
            }
        },
    }
}
