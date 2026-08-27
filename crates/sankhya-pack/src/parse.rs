//! Turning an expression's text into an [`Expr`].
//!
//! A precedence-climbing parser over a hand-written tokeniser. Small, because the language
//! is small: there is no statement, no block, no assignment and no call, so there is
//! nothing here to get subtly wrong beyond precedence itself --- which the tests pin
//! directly rather than by example.

use crate::expr::{BinaryOp, Expr, UnaryOp};
use sankhya_ext::value::Value;

/// Why an expression could not be parsed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    /// What went wrong.
    pub detail: String,
    /// How far into the text the parser was.
    pub at: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at character {})", self.detail, self.at)
    }
}

impl std::error::Error for ParseError {}

/// One lexical item.
#[derive(Clone, PartialEq, Debug)]
enum Token {
    Number(f64),
    Integer(i64),
    Text(String),
    Name(String),
    Operator(String),
    OpenParen,
    CloseParen,
}

/// Split text into tokens.
fn tokenise(text: &str) -> Result<Vec<(Token, usize)>, ParseError> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        let Some(c) = chars.get(i).copied() else {
            break;
        };
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;

        if c == '(' {
            out.push((Token::OpenParen, start));
            i += 1;
        } else if c == ')' {
            out.push((Token::CloseParen, start));
            i += 1;
        } else if c == '\'' {
            // A single-quoted string, with '' as an escaped quote, as SQL does it.
            i += 1;
            let mut value = String::new();
            loop {
                let Some(ch) = chars.get(i).copied() else {
                    return Err(ParseError {
                        detail: "unterminated string".to_string(),
                        at: start,
                    });
                };
                i += 1;
                if ch == '\'' {
                    if chars.get(i).copied() == Some('\'') {
                        value.push('\'');
                        i += 1;
                        continue;
                    }
                    break;
                }
                value.push(ch);
            }
            out.push((Token::Text(value), start));
        } else if c.is_ascii_digit() {
            let mut value = String::new();
            let mut seen_point = false;
            while let Some(ch) = chars.get(i).copied() {
                if ch.is_ascii_digit() {
                    value.push(ch);
                    i += 1;
                } else if ch == '.' && !seen_point {
                    seen_point = true;
                    value.push(ch);
                    i += 1;
                } else {
                    break;
                }
            }
            if seen_point {
                let parsed = value.parse::<f64>().map_err(|_| ParseError {
                    detail: format!("'{value}' is not a number"),
                    at: start,
                })?;
                out.push((Token::Number(parsed), start));
            } else {
                let parsed = value.parse::<i64>().map_err(|_| ParseError {
                    detail: format!("'{value}' is not an integer"),
                    at: start,
                })?;
                out.push((Token::Integer(parsed), start));
            }
        } else if c.is_alphabetic() || c == '_' {
            let mut value = String::new();
            while let Some(ch) = chars.get(i).copied() {
                if ch.is_alphanumeric() || ch == '_' {
                    value.push(ch);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push((Token::Name(value), start));
        } else {
            // Two-character operators first, so `<=` is not read as `<` then `=`.
            let two: String = chars
                .get(i..i + 2)
                .map(|s| s.iter().collect())
                .unwrap_or_default();
            if matches!(two.as_str(), "<=" | ">=" | "<>" | "!=") {
                out.push((Token::Operator(two), start));
                i += 2;
            } else if "+-*/<>=".contains(c) {
                out.push((Token::Operator(c.to_string()), start));
                i += 1;
            } else {
                return Err(ParseError {
                    detail: format!("'{c}' is not part of any expression"),
                    at: start,
                });
            }
        }
    }
    Ok(out)
}

/// Parse an expression.
pub fn parse(text: &str) -> Result<Expr, ParseError> {
    let tokens = tokenise(text)?;
    if tokens.is_empty() {
        return Err(ParseError {
            detail: "the expression is empty".to_string(),
            at: 0,
        });
    }
    let mut parser = Parser { tokens, at: 0 };
    let expr = parser.expression(0)?;
    if parser.at < parser.tokens.len() {
        return Err(ParseError {
            detail: "there is text after the end of the expression".to_string(),
            at: parser.position(),
        });
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<(Token, usize)>,
    at: usize,
}

impl Parser {
    fn position(&self) -> usize {
        self.tokens.get(self.at).map_or(0, |(_, p)| *p)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at).map(|(t, _)| t)
    }

    /// Precedence climbing. `minimum` is the lowest binding power this call will accept.
    fn expression(&mut self, minimum: u8) -> Result<Expr, ParseError> {
        let mut left = self.prefix()?;

        while let Some(token) = self.peek() {
            let Some((op, power)) = binary_of(token) else {
                break;
            };
            if power < minimum {
                break;
            }
            self.at += 1;
            // Left-associative: the right side binds one level tighter, so `a - b - c` is
            // `(a - b) - c` rather than `a - (b - c)`. Getting this backwards is silent for
            // addition and wrong for subtraction and division.
            let right = self.expression(power + 1)?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn prefix(&mut self) -> Result<Expr, ParseError> {
        let position = self.position();
        let Some(token) = self.peek().cloned() else {
            return Err(ParseError {
                detail: "the expression ends where a value was expected".to_string(),
                at: position,
            });
        };
        self.at += 1;

        match token {
            Token::Integer(n) => Ok(Expr::Literal(Value::Integer(n))),
            Token::Number(x) => Ok(Expr::Literal(Value::Real(x))),
            Token::Text(s) => Ok(Expr::Literal(Value::Text(s))),
            Token::OpenParen => {
                let inner = self.expression(0)?;
                if self.peek() != Some(&Token::CloseParen) {
                    return Err(ParseError {
                        detail: "expected a closing parenthesis".to_string(),
                        at: self.position(),
                    });
                }
                self.at += 1;
                Ok(inner)
            }
            Token::Operator(op) if op == "-" => Ok(Expr::Unary {
                op: UnaryOp::Negate,
                operand: Box::new(self.expression(7)?),
            }),
            Token::Name(name) => match name.to_lowercase().as_str() {
                "not" => Ok(Expr::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(self.expression(3)?),
                }),
                "true" => Ok(Expr::Literal(Value::Boolean(true))),
                "false" => Ok(Expr::Literal(Value::Boolean(false))),
                "null" => Ok(Expr::Literal(Value::Null)),
                "if" => self.conditional(),
                _ => Ok(Expr::Argument(name)),
            },
            other => Err(ParseError {
                detail: format!("{other:?} cannot begin an expression"),
                at: position,
            }),
        }
    }

    /// `if condition then a else b`.
    fn conditional(&mut self) -> Result<Expr, ParseError> {
        let condition = self.expression(0)?;
        self.keyword("then")?;
        let then = self.expression(0)?;
        self.keyword("else")?;
        let otherwise = self.expression(0)?;
        Ok(Expr::IfElse {
            condition: Box::new(condition),
            then: Box::new(then),
            otherwise: Box::new(otherwise),
        })
    }

    fn keyword(&mut self, wanted: &str) -> Result<(), ParseError> {
        let position = self.position();
        match self.peek() {
            Some(Token::Name(name)) if name.eq_ignore_ascii_case(wanted) => {
                self.at += 1;
                Ok(())
            }
            _ => Err(ParseError {
                detail: format!("expected '{wanted}'"),
                at: position,
            }),
        }
    }
}

/// The operator a token denotes, and how tightly it binds.
///
/// Higher binds tighter. The levels mirror SQL's, which is what anyone writing one of these
/// will expect: `or` loosest, then `and`, then comparison, then `+ -`, then `* /`.
fn binary_of(token: &Token) -> Option<(BinaryOp, u8)> {
    let text = match token {
        Token::Operator(op) => op.as_str(),
        Token::Name(name) => name.as_str(),
        _ => return None,
    };
    Some(match text.to_lowercase().as_str() {
        "or" => (BinaryOp::Or, 1),
        "and" => (BinaryOp::And, 2),
        "=" => (BinaryOp::Equal, 4),
        "<>" | "!=" => (BinaryOp::NotEqual, 4),
        "<" => (BinaryOp::Less, 4),
        "<=" => (BinaryOp::LessOrEqual, 4),
        ">" => (BinaryOp::Greater, 4),
        ">=" => (BinaryOp::GreaterOrEqual, 4),
        "+" => (BinaryOp::Add, 5),
        "-" => (BinaryOp::Subtract, 5),
        "*" => (BinaryOp::Multiply, 6),
        "/" => (BinaryOp::Divide, 6),
        _ => return None,
    })
}
