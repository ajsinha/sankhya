//! Deterministic row generation.

use crate::schema::{Column, ColumnKind, Schema};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// How much data to produce.
#[derive(Clone, Copy, Debug)]
pub struct Scale {
    /// Approximate uncompressed bytes across all tables.
    pub total_bytes: u64,
    /// How many tables to use, taken from the front of the schema list.
    pub tables: usize,
}

impl Scale {
    /// The scale the acceptance run uses: ten tables, ten gigabytes.
    #[must_use]
    pub const fn acceptance() -> Self {
        Self {
            total_bytes: 10 * 1024 * 1024 * 1024,
            tables: 10,
        }
    }

    /// A small scale for fast tests.
    #[must_use]
    pub const fn smoke() -> Self {
        Self {
            total_bytes: 4 * 1024 * 1024,
            tables: 10,
        }
    }
}

/// A batch of generated rows, rendered as text values in column order.
///
/// Text rather than typed values because the generator's job is to feed the *source*
/// database, which parses text. The typed round trip is the pipeline's responsibility
/// and is exactly what reconciliation checks.
#[derive(Clone, Debug)]
pub struct RowBatch {
    pub schema: &'static str,
    pub columns: &'static [Column],
    pub rows: Vec<Vec<Option<String>>>,
}

impl RowBatch {
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A seeded generator.
///
/// The same seed and the same row sequence produce byte-identical output on any
/// machine, which is what lets a test recompute expected state instead of storing it.
#[derive(Debug)]
pub struct Generator {
    seed: u64,
}

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz ";
const HEX: &[u8] = b"0123456789abcdef";

impl Generator {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// A stream position's own generator, so any row can be produced independently of
    /// the ones before it. That makes generation parallelisable and, more importantly,
    /// makes a single row's expected value checkable without replaying the run.
    fn rng_for(&self, table_index: u64, row: u64) -> ChaCha8Rng {
        // Mix the three coordinates so adjacent rows do not share a stream.
        let mixed = self
            .seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(table_index.wrapping_mul(0xBF58_476D_1CE4_E5B9))
            .wrapping_add(row.wrapping_mul(0x94D0_49BB_1331_11EB));
        ChaCha8Rng::seed_from_u64(mixed)
    }

    /// How many rows each table needs to reach the requested total.
    ///
    /// Divided by uncompressed row width rather than evenly by row count, so a wide
    /// table does not dominate the run.
    #[must_use]
    pub fn plan(&self, scale: Scale) -> Vec<(&'static Schema, u64)> {
        let all = crate::schema::all_schemas();
        let schemas = all.get(..scale.tables.min(all.len())).unwrap_or(all);
        let per_table = scale.total_bytes / schemas.len().max(1) as u64;
        schemas
            .iter()
            .map(|s| (s, s.rows_for_bytes(per_table).max(1)))
            .collect()
    }

    /// Generate a contiguous batch of rows for one table.
    #[must_use]
    pub fn batch(
        &self,
        schema: &'static Schema,
        table_index: u64,
        from_row: u64,
        count: u64,
    ) -> RowBatch {
        let rows = (from_row..from_row.saturating_add(count))
            .map(|row| {
                let mut rng = self.rng_for(table_index, row);
                schema
                    .columns
                    .iter()
                    .map(|c| Self::value(c, row, &mut rng))
                    .collect()
            })
            .collect();
        RowBatch {
            schema: schema.name,
            columns: schema.columns,
            rows,
        }
    }

    fn value(column: &Column, row: u64, rng: &mut ChaCha8Rng) -> Option<String> {
        // Nulls are deliberate and reproducible: roughly one in eleven nullable
        // values, which is frequent enough that null handling is genuinely exercised
        // rather than incidentally.
        if column.nullable && row % 11 == 3 {
            return None;
        }
        Some(match column.kind {
            ColumnKind::Serial => (row + 1).to_string(),
            ColumnKind::Category { cardinality } => {
                format!("cat-{:05}", rng.random_range(0..cardinality))
            }
            ColumnKind::Identifier { cardinality } => {
                format!("id-{:09}", rng.random_range(0..cardinality))
            }
            ColumnKind::Text { mean_len } => {
                let len = mean_len.saturating_sub(8) + rng.random_range(0..16);
                random_text(rng, len as usize)
            }
            // Incompressible, so the source stores it out-of-line and withholds it on
            // an update that leaves it alone. That is the case the decoder must handle.
            ColumnKind::LargePayload { len } => random_hex(rng, len as usize),
            ColumnKind::Decimal { precision, scale } => {
                let digits = precision.saturating_sub(scale).min(15);
                let whole: u64 =
                    rng.random_range(0..10u64.saturating_pow(u32::from(digits)).max(1));
                let frac: u64 = rng.random_range(0..10u64.saturating_pow(u32::from(scale)).max(1));
                format!("{whole}.{frac:0width$}", width = usize::from(scale))
            }
            ColumnKind::Real => format!("{:.6}", rng.random_range(-1.0e6..1.0e6f64)),
            ColumnKind::Integer { min, max } => rng.random_range(min..=max).to_string(),
            ColumnKind::Boolean => if rng.random_bool(0.5) {
                "true"
            } else {
                "false"
            }
            .to_string(),
            // Increases with the row sequence, so the natural sort order is also the
            // arrival order — which is what makes commit-position sorting free.
            ColumnKind::Timestamp => {
                let base = 1_735_689_600i64; // 2025-01-01T00:00:00Z
                let offset = (row as i64).saturating_mul(37) + rng.random_range(0..37i64);
                format_timestamp(base + offset)
            }
            ColumnKind::Date => {
                let base = 1_735_689_600i64;
                let offset = (row as i64).saturating_mul(37);
                format_date(base + offset)
            }
            ColumnKind::Json => format!(
                r#"{{"source":"gen","seq":{row},"score":{},"flag":{}}}"#,
                rng.random_range(0..1000),
                rng.random_bool(0.5)
            ),
        })
    }
}

fn random_text(rng: &mut ChaCha8Rng, len: usize) -> String {
    (0..len)
        .map(|_| {
            let i = rng.random_range(0..ALPHABET.len());
            char::from(ALPHABET.get(i).copied().unwrap_or(b'a'))
        })
        .collect()
}

fn random_hex(rng: &mut ChaCha8Rng, len: usize) -> String {
    (0..len)
        .map(|_| {
            let i = rng.random_range(0..HEX.len());
            char::from(HEX.get(i).copied().unwrap_or(b'0'))
        })
        .collect()
}

/// Civil-time formatting without pulling a calendar dependency into a generator.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Days from the civil epoch, per Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (y, m as u32, d as u32, h as u32, mi as u32, s as u32)
}

fn format_timestamp(secs: i64) -> String {
    let (y, m, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}+00")
}

fn format_date(secs: i64) -> String {
    let (y, m, d, _, _, _) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}")
}
