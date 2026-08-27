//! A reference pack: device readings, thresholds and windows.
//!
//! Deliberately the **opposite** of `pack-ref-logistics`. That one is graph-heavy; this one
//! touches no graph at all and works entirely on scalar time-series values. Two packs from
//! unrelated industries exercising opposite halves of the extension surface is what tests
//! the surface rather than one well-worn path through it.
//!
//! It touches zero core files, which is the M4 exit criterion, and `check-layers` enforces
//! that mechanically rather than leaving it to review.

#![doc(html_root_url = "https://docs.rs/pack-ref-telemetry")]

use sankhya_ext::function::{
    Invocation, Pack, PackInfo, ScalarFunction, Signature, TableFunction, API_VERSION,
};
use sankhya_ext::registry::Registry;
use sankhya_ext::value::{LogicalType, Value};
use sankhya_ext::PackError;
use std::sync::Arc;

/// The telemetry reference pack.
#[derive(Debug, Default)]
pub struct TelemetryPack;

impl Pack for TelemetryPack {
    fn info(&self) -> PackInfo {
        PackInfo {
            name: "telemetry".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION,
            description: "Device readings, threshold breaches and window bucketing.".to_string(),
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.add_scalar(Arc::new(BreachSeverity));
        registry.add_scalar(Arc::new(WindowStart));
        registry.add_table(Arc::new(ThresholdBands));
    }
}

/// How far past a threshold a reading is, as a proportion of the threshold.
#[derive(Debug)]
struct BreachSeverity;

impl ScalarFunction for BreachSeverity {
    fn name(&self) -> &str {
        "telemetry_breach_severity"
    }

    fn description(&self) -> &str {
        "How far a reading exceeds a threshold, as a proportion of it."
    }

    fn signature(&self) -> Signature {
        Signature::of(
            vec![LogicalType::Real, LogicalType::Real],
            LogicalType::Real,
        )
    }

    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError> {
        let (Some(reading), Some(threshold)) = (arguments.first(), arguments.get(1)) else {
            return Err(context.invalid_argument("a reading and a threshold are required"));
        };
        if reading.is_null() || threshold.is_null() {
            return Ok(Value::Null);
        }
        let (Some(reading), Some(threshold)) = (reading.as_real(), threshold.as_real()) else {
            return Err(context.invalid_argument("both arguments must be numbers"));
        };
        if threshold == 0.0 {
            // A proportion of zero is not a large number, it is undefined. Returning
            // infinity would sort to the top of every "worst breaches" list.
            return Err(context.invalid_argument(
                "a threshold of zero has no proportion; the severity is undefined rather \
                 than infinite, and an infinite one would head every ranked result",
            ));
        }
        Ok(Value::Real(((reading - threshold) / threshold).max(0.0)))
    }
}

/// The start of the fixed window a given instant falls in.
#[derive(Debug)]
struct WindowStart;

impl ScalarFunction for WindowStart {
    fn name(&self) -> &str {
        "telemetry_window_start"
    }

    fn description(&self) -> &str {
        "The start of the fixed-width window containing an instant."
    }

    fn signature(&self) -> Signature {
        Signature::of(
            vec![LogicalType::Instant, LogicalType::Integer],
            LogicalType::Instant,
        )
    }

    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError> {
        let (Some(at), Some(width)) = (arguments.first(), arguments.get(1)) else {
            return Err(context.invalid_argument("an instant and a window width are required"));
        };
        if at.is_null() || width.is_null() {
            return Ok(Value::Null);
        }
        let (Some(at), Some(width)) = (at.as_instant(), width.as_integer()) else {
            return Err(context.invalid_argument("expected an instant and an integer"));
        };
        if width <= 0 {
            return Err(context.invalid_argument("the window width must be above zero"));
        }
        // Floor division, so negative instants bucket downward like positive ones. Rust's
        // `%` truncates toward zero, which would put an instant just before the epoch in
        // the window *after* the one containing it.
        Ok(Value::Instant(at - at.rem_euclid(width)))
    }
}

/// The severity bands a threshold implies, as rows.
///
/// A table function, so this pack exercises that half of the API too --- the logistics pack
/// contributes only scalars, and an API surface tested from one side is a surface with an
/// untested half.
#[derive(Debug)]
struct ThresholdBands;

impl TableFunction for ThresholdBands {
    fn name(&self) -> &str {
        "telemetry_threshold_bands"
    }

    fn description(&self) -> &str {
        "The severity bands implied by a threshold."
    }

    fn signature(&self) -> Signature {
        Signature::of(vec![LogicalType::Real], LogicalType::Any)
    }

    fn columns(&self) -> Vec<(String, LogicalType)> {
        vec![
            ("band".to_string(), LogicalType::Text),
            ("lower".to_string(), LogicalType::Real),
            ("upper".to_string(), LogicalType::Real),
        ]
    }

    fn estimated_rows(&self, _arguments: &[Value]) -> Option<usize> {
        Some(3)
    }

    fn invoke(
        &self,
        arguments: &[Value],
        context: &Invocation,
    ) -> Result<Vec<Vec<Value>>, PackError> {
        let Some(threshold) = arguments.first().and_then(Value::as_real) else {
            return Err(context.invalid_argument("a numeric threshold is required"));
        };
        if threshold <= 0.0 {
            return Err(context.invalid_argument("the threshold must be above zero"));
        }
        context.check()?;
        Ok(vec![
            vec![
                Value::Text("nominal".to_string()),
                Value::Real(0.0),
                Value::Real(threshold),
            ],
            vec![
                Value::Text("elevated".to_string()),
                Value::Real(threshold),
                Value::Real(threshold * 1.5),
            ],
            vec![
                Value::Text("critical".to_string()),
                Value::Real(threshold * 1.5),
                Value::Real(f64::INFINITY),
            ],
        ])
    }
}
