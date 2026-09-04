//! The warehouse an adversarial reviewer is given, which is deliberately awkward.
//!
//! Its own binary rather than a second `#[ignore]`d test beside the documented recipe.
//!
//! # Why that mattered
//!
//! `docs/QUICKSTART.md` tells a reader to run
//! `cargo test -p sankhya-server --test make_warehouse -- --ignored`, and `--ignored` runs
//! **every** ignored test in the binary. Both lived in one file, so the documented command
//! wrote eleven tables across seven schemas rather than the one the document described ---
//! and three of them were called `orders`, on purpose, because an ambiguous bare name is a
//! case worth handing a reviewer.
//!
//! The consequence was that QUICKSTART's own query failed against QUICKSTART's own
//! warehouse, on ambiguity, using a fixture built to *create* that ambiguity. Separated, so
//! the documented command does exactly what the document says.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

/// A `common` schema every reviewer may read, and `probe_a` .. `probe_e`, one per reviewer.
/// Two tables share the name `orders` --- in `common` and in `probe_a` --- deliberately: an
/// ambiguous bare name is a live case here rather than a unit test, and a reviewer who finds a
/// path that resolves it to one of the two has found something worth knowing.
#[test]
#[ignore = "writes a warehouse to SANKHYA_WAREHOUSE; run deliberately"]
fn write_a_review_warehouse() {
    let root = std::path::PathBuf::from(
        std::env::var("SANKHYA_WAREHOUSE").expect("set SANKHYA_WAREHOUSE"),
    );

    // The quarantine, so a feed has somewhere to put a record that does not fit.
    let quarantine = root.join("sank").join(sankhya_feed::quarantine::TABLE);
    Publication::external(&quarantine, sankhya_feed::quarantine::TABLE)
        .create(&sankhya_feed::quarantine::schema())
        .expect("creating the quarantine");

    let wide = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("period", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
        Field::new("note", DataType::Utf8, true),
    ]));

    let mut written = Vec::new();
    for (schema_name, table_name, files) in [
        ("common", "orders", 4u64),
        ("common", "regions", 1),
        ("probe_a", "orders", 2),
        ("probe_a", "scratch", 1),
        ("probe_b", "scratch", 1),
        ("probe_c", "scratch", 1),
        ("probe_d", "scratch", 1),
        ("probe_e", "scratch", 1),
        // A table with a log and no rows at all. An empty table has its own read path, and it
        // is the one that broke on a projected query when nobody had ever run one.
        ("common", "empty", 0),
    ] {
        let table_root = root.join(schema_name).join(table_name);
        let publication = Publication::external(&table_root, table_name);
        publication.create(&wide).expect("creating");

        for file in 0..files {
            let base = i64::try_from(file).unwrap_or(0) * 250;
            let ids: Vec<i64> = (0..250).map(|i| base + i).collect();
            let regions: Vec<Option<&str>> = ids
                .iter()
                .map(|i| match i % 3 {
                    0 => Some("north"),
                    1 => Some("south"),
                    // A null in every third row, so a reviewer testing null handling has one.
                    _ => None,
                })
                .collect();
            let periods: Vec<Option<&str>> = ids
                .iter()
                .map(|i| if i % 2 == 0 { Some("q1") } else { Some("q2") })
                .collect();
            #[allow(clippy::cast_precision_loss)]
            let amounts: Vec<f64> = ids.iter().map(|i| *i as f64 * 1.5).collect();
            let notes: Vec<Option<String>> = ids
                .iter()
                .map(|i| match i % 7 {
                    // Text a naive renderer would break on: quotes, a backslash, a newline,
                    // and something outside ASCII.
                    0 => Some("it's \"quoted\"".to_owned()),
                    1 => Some("back\\slash".to_owned()),
                    2 => Some("two\nlines".to_owned()),
                    3 => Some("naïve café — ünïcode".to_owned()),
                    4 => Some(String::new()),
                    _ => None,
                })
                .collect();

            let batch = RecordBatch::try_new(
                Arc::clone(&wide),
                vec![
                    Arc::new(Int64Array::from(ids)),
                    Arc::new(StringArray::from(regions)),
                    Arc::new(StringArray::from(periods)),
                    Arc::new(Float64Array::from(amounts)),
                    Arc::new(StringArray::from(notes)),
                ],
            )
            .expect("a valid batch");

            publication
                .append(
                    file + 1,
                    &format!("part-{file:04}.parquet"),
                    &batch,
                    Lsn::new((file + 1) * 250),
                )
                .expect("publishing");
        }
        written.push(format!("{schema_name}.{table_name} ({files} file(s))"));
    }
    println!("wrote {} table(s):", written.len());
    for line in written {
        println!("  {line}");
    }
}
