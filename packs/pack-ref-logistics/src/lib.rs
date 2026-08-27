//! A reference pack: shipments, depots and routes.
//!
//! Deliberately non-financial, and deliberately **graph-heavy** --- its counterpart,
//! `pack-ref-telemetry`, is graph-free. Two packs from unrelated industries exercising
//! opposite halves of the extension surface is what tests the surface rather than one
//! well-worn path through it.
//!
//! # What this pack is evidence of
//!
//! It touches zero core files. That is the M4 exit criterion, and it is enforced
//! mechanically: `cargo xtask check-layers` refuses any dependency from a pack on anything
//! but `sankhya-ext`, `sankhya-types` and `sankhya-error`, and refuses any dependency from
//! a core crate on a pack. If this pack needed something the API does not offer, the build
//! would fail --- and that failure would be the signal to widen the API, not the allowance.
//!
//! Nothing here is imported by the engine. The engine is handed a `&dyn Pack` and asks it
//! what it has.

#![doc(html_root_url = "https://docs.rs/pack-ref-logistics")]

use sankhya_ext::function::{Invocation, Pack, PackInfo, ScalarFunction, Signature, API_VERSION};
use sankhya_ext::registry::Registry;
use sankhya_ext::value::{LogicalType, Value};
use sankhya_ext::PackError;
use std::sync::Arc;

/// The logistics reference pack.
#[derive(Debug, Default)]
pub struct LogisticsPack;

impl Pack for LogisticsPack {
    fn info(&self) -> PackInfo {
        PackInfo {
            name: "logistics".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION,
            description: "Shipment routing, depot dwell and consignment integrity.".to_string(),
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.add_scalar(Arc::new(DwellHours));
        registry.add_scalar(Arc::new(RouteLegCost));
        registry.add_scalar(Arc::new(ConsignmentCheckDigit));
    }
}

/// How many hours a shipment sat at a depot.
#[derive(Debug)]
struct DwellHours;

impl ScalarFunction for DwellHours {
    fn name(&self) -> &str {
        "logistics_dwell_hours"
    }

    fn description(&self) -> &str {
        "Hours between arrival at a depot and departure from it."
    }

    fn signature(&self) -> Signature {
        Signature::of(
            vec![LogicalType::Instant, LogicalType::Instant],
            LogicalType::Real,
        )
    }

    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError> {
        let (Some(first), Some(second)) = (arguments.first(), arguments.get(1)) else {
            return Err(context.invalid_argument("two instants are required"));
        };
        // Null in, null out. Substituting zero would turn "we do not know when it left"
        // into "it left immediately", which is a different claim entirely.
        if first.is_null() || second.is_null() {
            return Ok(Value::Null);
        }
        let (Some(arrived), Some(departed)) = (first.as_instant(), second.as_instant()) else {
            return Err(context.invalid_argument("both arguments must be instants"));
        };
        if departed < arrived {
            return Err(context.invalid_argument(
                "the departure precedes the arrival; a negative dwell is a data defect \
                 rather than a small number",
            ));
        }
        #[allow(clippy::cast_precision_loss)]
        let hours = (departed - arrived) as f64 / 3_600_000_000.0;
        Ok(Value::Real(hours))
    }
}

/// What one leg of a route costs, given distance and a per-unit rate.
#[derive(Debug)]
struct RouteLegCost;

impl ScalarFunction for RouteLegCost {
    fn name(&self) -> &str {
        "logistics_leg_cost"
    }

    fn description(&self) -> &str {
        "Cost of a route leg from its distance and rate."
    }

    fn signature(&self) -> Signature {
        Signature::of(
            vec![LogicalType::Real, LogicalType::Real],
            LogicalType::Real,
        )
    }

    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError> {
        let (Some(distance), Some(rate)) = (arguments.first(), arguments.get(1)) else {
            return Err(context.invalid_argument("a distance and a rate are required"));
        };
        if distance.is_null() || rate.is_null() {
            return Ok(Value::Null);
        }
        let (Some(distance), Some(rate)) = (distance.as_real(), rate.as_real()) else {
            return Err(context.invalid_argument("both arguments must be numbers"));
        };
        if distance < 0.0 {
            return Err(context.invalid_argument("a negative distance is a data defect"));
        }
        Ok(Value::Real(distance * rate))
    }
}

/// The check digit of a consignment reference.
///
/// A loop, so it is the pack function used to demonstrate cooperative cancellation. Its work
/// is bounded by the input length, but it calls [`Invocation::check`] anyway --- which is the
/// habit every looping pack function should have.
#[derive(Debug)]
struct ConsignmentCheckDigit;

impl ScalarFunction for ConsignmentCheckDigit {
    fn name(&self) -> &str {
        "logistics_check_digit"
    }

    fn description(&self) -> &str {
        "The modulus-11 check digit of a consignment reference."
    }

    fn signature(&self) -> Signature {
        Signature::of(vec![LogicalType::Text], LogicalType::Integer)
    }

    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError> {
        let Some(value) = arguments.first() else {
            return Err(context.invalid_argument("a reference is required"));
        };
        if value.is_null() {
            return Ok(Value::Null);
        }
        let Some(text) = value.as_text() else {
            return Err(context.invalid_argument("the reference must be text"));
        };

        let mut total: i64 = 0;
        for (index, byte) in text.bytes().enumerate() {
            // Every thousand characters, in case someone passes something enormous. A
            // looping function that never checks is one the engine has to abandon.
            if index % 1_000 == 0 {
                context.check()?;
            }
            if !byte.is_ascii_digit() {
                return Err(context.invalid_argument(format!(
                    "the reference contains '{}', which is not a digit",
                    byte as char
                )));
            }
            let digit = i64::from(byte - b'0');
            let weight = i64::try_from(index % 6).unwrap_or(0) + 2;
            total += digit * weight;
        }
        Ok(Value::Integer((11 - (total % 11)) % 11))
    }
}
