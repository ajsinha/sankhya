//! Emit schema definitions and bulk-load data for the synthetic fixture set.
//!
//! Writes to standard output so it can be piped straight into a database client,
//! which keeps the generator free of any database dependency and makes the load path
//! the same one an operator would use.
//!
//! ```text
//! sankhya-datagen ddl                       > schema.sql
//! sankhya-datagen copy --gb 10 --tables 10  | psql ...
//! ```

use sankhya_datagen::{ColumnKind, Generator, Scale, Schema, all_schemas};
use std::io::{self, BufWriter, Write};

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    let gb = flag(&args, "--gb").and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.0);
    let tables = flag(&args, "--tables").and_then(|v| v.parse::<usize>().ok()).unwrap_or(10);
    let seed = flag(&args, "--seed").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0x5A4E_4B48_5941_0001);

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scale = Scale {
        total_bytes: (gb * 1024.0 * 1024.0 * 1024.0) as u64,
        tables,
    };

    let out = io::stdout();
    let mut w = BufWriter::with_capacity(1 << 20, out.lock());

    match command {
        "ddl" => emit_ddl(&mut w, scale.tables),
        "copy" => emit_copy(&mut w, Generator::new(seed), scale),
        "plan" => emit_plan(&mut w, Generator::new(seed), scale),
        _ => {
            eprintln!(
                "usage:\n  \
                 sankhya-datagen ddl   [--tables N]\n  \
                 sankhya-datagen plan  [--gb G] [--tables N] [--seed S]\n  \
                 sankhya-datagen copy  [--gb G] [--tables N] [--seed S]"
            );
            Ok(())
        }
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Map a logical column shape onto source types.
///
/// Deliberately exercises the types that are awkward to carry faithfully — exact
/// decimals, timestamps with zone, structured documents, and out-of-line payloads.
fn sql_type(kind: ColumnKind) -> String {
    match kind {
        ColumnKind::Serial => "bigint".into(),
        ColumnKind::Category { .. } | ColumnKind::Identifier { .. } => "text".into(),
        ColumnKind::Text { .. } | ColumnKind::LargePayload { .. } => "text".into(),
        ColumnKind::Decimal { precision, scale } => format!("numeric({precision},{scale})"),
        ColumnKind::Real => "double precision".into(),
        ColumnKind::Integer { .. } => "bigint".into(),
        ColumnKind::Boolean => "boolean".into(),
        ColumnKind::Timestamp => "timestamptz".into(),
        ColumnKind::Date => "date".into(),
        ColumnKind::Json => "jsonb".into(),
    }
}

fn emit_ddl(w: &mut impl Write, tables: usize) -> io::Result<()> {
    writeln!(w, "-- SANKHYA synthetic fixture schema. Generated; do not edit.")?;
    writeln!(w, "-- Deliberately non-financial: the general-purpose claim is tested, not asserted.\n")?;
    for schema in all_schemas().iter().take(tables) {
        writeln!(w, "DROP TABLE IF EXISTS {} CASCADE;", schema.name)?;
        writeln!(w, "CREATE TABLE {} (", schema.name)?;
        let last = schema.columns.len().saturating_sub(1);
        for (i, column) in schema.columns.iter().enumerate() {
            let null = if column.nullable { "" } else { " NOT NULL" };
            let pk = if matches!(column.kind, ColumnKind::Serial) { " PRIMARY KEY" } else { "" };
            let comma = if i == last { "" } else { "," };
            writeln!(w, "    {:<16} {}{null}{pk}{comma}", column.name, sql_type(column.kind))?;
        }
        writeln!(w, ");")?;

        // Out-of-line storage, uncompressed. Without this the payload compresses and
        // stays inline, and the withheld-value path is never exercised.
        for column in schema.columns.iter().filter(|c| matches!(c.kind, ColumnKind::LargePayload { .. })) {
            writeln!(w, "ALTER TABLE {} ALTER COLUMN {} SET STORAGE EXTERNAL;", schema.name, column.name)?;
        }
        writeln!(w)?;
    }
    Ok(())
}

fn emit_plan(w: &mut impl Write, generator: Generator, scale: Scale) -> io::Result<()> {
    writeln!(w, "{:<22} {:>14} {:>12} {:>14}", "table", "rows", "bytes/row", "approx bytes")?;
    let plan = generator.plan(scale);
    let mut total_rows = 0u64;
    let mut total_bytes = 0u64;
    for (schema, rows) in &plan {
        let bytes = schema.approx_row_bytes() * rows;
        total_rows += rows;
        total_bytes += bytes;
        writeln!(w, "{:<22} {rows:>14} {:>12} {bytes:>14}", schema.name, schema.approx_row_bytes())?;
    }
    writeln!(w, "{:<22} {total_rows:>14} {:>12} {total_bytes:>14}", "TOTAL", "")?;
    writeln!(w, "\n-- {:.2} GiB uncompressed across {} tables", total_bytes as f64 / 1024.0 / 1024.0 / 1024.0, plan.len())?;
    Ok(())
}

/// Emit a `COPY ... FROM STDIN` stream per table.
///
/// Text format with explicit escaping. Batched so memory stays flat regardless of the
/// requested volume — a ten-gigabyte run must not need ten gigabytes of memory.
fn emit_copy(w: &mut impl Write, generator: Generator, scale: Scale) -> io::Result<()> {
    const BATCH: u64 = 20_000;
    for (table_index, (schema, rows)) in generator.plan(scale).into_iter().enumerate() {
        let columns: Vec<&str> = schema.columns.iter().map(|c| c.name).collect();
        writeln!(w, "COPY {} ({}) FROM STDIN;", schema.name, columns.join(", "))?;
        let mut written = 0u64;
        while written < rows {
            let count = BATCH.min(rows - written);
            let batch = generator.batch(schema, table_index as u64, written, count);
            for row in &batch.rows {
                write_row(w, row)?;
            }
            written += count;
        }
        writeln!(w, "\\.")?;
    }
    Ok(())
}

fn write_row(w: &mut impl Write, row: &[Option<String>]) -> io::Result<()> {
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            w.write_all(b"\t")?;
        }
        match value {
            // The text format's null marker. A literal backslash-N in data would be
            // ambiguous, which is why the escaper below never emits one unescaped.
            None => w.write_all(b"\\N")?,
            Some(v) => write_escaped(w, v)?,
        }
    }
    w.write_all(b"\n")
}

/// Escape the characters that are structural in the text copy format.
///
/// Getting this wrong corrupts the load silently rather than failing it, which is
/// worse than a parse error.
fn write_escaped(w: &mut impl Write, value: &str) -> io::Result<()> {
    if !value.bytes().any(|b| matches!(b, b'\\' | b'\n' | b'\r' | b'\t')) {
        return w.write_all(value.as_bytes());
    }
    for byte in value.bytes() {
        match byte {
            b'\\' => w.write_all(b"\\\\")?,
            b'\n' => w.write_all(b"\\n")?,
            b'\r' => w.write_all(b"\\r")?,
            b'\t' => w.write_all(b"\\t")?,
            other => w.write_all(&[other])?,
        }
    }
    Ok(())
}

/// Reference to the schema type so the import is used in signature position.
const _: fn(&Schema) -> u64 = |s| s.approx_row_bytes();
