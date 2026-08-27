//! Not a test: a way to write a real warehouse to a known path, so the server can be
//! started against it by hand and driven with a real client.
//!
//! Ignored by default, because it writes to a path given in the environment, and a test
//! that writes outside its own temporary directory is one that surprises somebody.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::sync::Arc;

#[test]
#[ignore = "writes a warehouse to SANKHYA_WAREHOUSE; run deliberately"]
fn write_a_warehouse() {
    let root = std::path::PathBuf::from(
        std::env::var("SANKHYA_WAREHOUSE").expect("set SANKHYA_WAREHOUSE"),
    );

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
    ]));

    let table_root = root.join("sales").join("orders");
    std::fs::create_dir_all(&table_root).expect("creating");

    let delta = sankhya_table_delta::schema_string(&schema).expect("representable");
    commit(&table_root, 0, &create(Metadata::new("orders", delta, 0))).expect("creating");

    // Several files, so the read path has something to prune and to parallelise over.
    let mut adds = Vec::new();
    for file in 0..4u64 {
        let ids: Vec<i64> = (0..250)
            .map(|i| i64::try_from(file * 250 + i).unwrap())
            .collect();
        let regions: Vec<Option<&str>> = ids
            .iter()
            .map(|i| match i % 3 {
                0 => Some("north"),
                1 => Some("south"),
                _ => None,
            })
            .collect();
        let amounts: Vec<f64> = ids
            .iter()
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                {
                    *i as f64 * 1.5
                }
            })
            .collect();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(regions)),
                Arc::new(Float64Array::from(amounts)),
            ],
        )
        .expect("a valid batch");

        let name = format!("part-{file:04}.parquet");
        let report = write_parquet(
            &table_root,
            &name,
            &batch,
            Lsn::new((file + 1) * 250),
            WriterConfig::default(),
        )
        .expect("writing");
        adds.push(Action::Add(AddFile::with_rows(name, report.bytes, 0, 250)));
    }
    commit(&table_root, 1, &adds).expect("publishing");
    println!("wrote {}", table_root.display());
}
