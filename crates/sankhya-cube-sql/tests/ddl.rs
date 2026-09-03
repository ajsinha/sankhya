//! `CREATE CUBE` and `DROP CUBE`, read from the text a client sends.
//!
//! The property under test that matters most is not that a good statement parses. It is that
//! a statement which is **not** cube DDL is left entirely alone, because this parser runs
//! before the engine and every other statement in the language has to get past it untouched.

// Tests may panic — that is how a test reports a failure. The workspace denies `unwrap`,
// `expect`, `panic` and indexing because a *server* must not do those things on data it did
// not choose; a test chooses all of its data, and an assertion that cannot fail loudly is
// worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube_algo::measure::Rule;
use sankhya_cube_sql::ddl::{parse, Statement};

/// A cube exercising every clause the grammar has.
const FULL: &str = "
CREATE CUBE sales FROM orders
  DIMENSION geography FROM regions ON region_id (
      LEVEL country = country_code,
      LEVEL region  = region_code,
      ROLLUP emea TO world
  )
  DIMENSION period FROM calendar ON order_date (
      LEVEL year    = year_number,
      LEVEL quarter = quarter_number
  )
  MEASURE amount  (SUM ALONG geography, SUM ALONG period)
  MEASURE balance (SUM ALONG geography, LAST ALONG period)
  MAINTAINED WITHIN 5 VERSIONS
  PINNED (geography)
";

fn created(sql: &str) -> sankhya_cube::model::Definition {
    match parse(sql) {
        Some(Ok(Statement::Create(definition))) => *definition,
        other => panic!("expected a CREATE CUBE, got {other:?}"),
    }
}

#[test]
fn a_definition_survives_the_trip_from_text_to_a_validated_cube() {
    // The whole point of the statement: what a client types has to reach the same type a
    // file on disk produces, and be accepted by the same validator.
    let definition = created(FULL);

    assert_eq!(definition.name, "sales");
    assert_eq!(definition.fact_table, "orders");
    assert_eq!(definition.dimensions.len(), 2);
    assert_eq!(definition.measures.len(), 2);
    assert_eq!(definition.target_lag, Some(5));
    assert_eq!(definition.pinned, vec![vec!["geography".to_string()]]);

    let cube = definition.validate().expect("the definition describes a usable cube");
    assert_eq!(cube.name(), "sales");
    assert_eq!(cube.target_lag(), Some(5));
}

#[test]
fn levels_keep_the_order_they_were_written_in() {
    // Declaration order *is* the hierarchy, coarse to fine, and it is deliberately not
    // inferred from cardinality. A parser that collected levels into anything unordered
    // would reverse a hierarchy on some other axis entirely.
    let definition = created(FULL);
    let geography = &definition.dimensions[0];

    assert_eq!(geography.name, "geography");
    assert_eq!(geography.table, "regions");
    assert_eq!(geography.joins_on, "region_id");
    let names: Vec<&str> = geography.levels.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["country", "region"]);
    assert_eq!(geography.levels[0].column, "country_code");
}

#[test]
fn a_rule_is_read_per_dimension_and_is_never_defaulted() {
    // A missing rule is a definition error rather than an implicit Sum, and that is the
    // whole design of the measure model. This test pins the half the parser owns: that the
    // rules it *did* read are attached to the dimensions they were written against.
    let definition = created(FULL);
    let balance = &definition.measures[1];

    assert_eq!(balance.name, "balance");
    assert_eq!(balance.rule("geography"), Some(Rule::Sum));
    assert_eq!(balance.rule("period"), Some(Rule::Last), "a balance does not add over time");
}

#[test]
fn every_rule_the_model_declares_can_be_written() {
    // A rule the algebra has and the grammar cannot express is a cube that a file can
    // declare and a statement cannot, which is the gap this module exists to close.
    for (written, expected) in [
        ("SUM", Rule::Sum),
        ("LAST", Rule::Last),
        ("FIRST", Rule::First),
        ("MAX", Rule::Max),
        ("MIN", Rule::Min),
        ("MEAN", Rule::Mean),
        ("NONE", Rule::None),
    ] {
        let sql = format!(
            "CREATE CUBE c FROM f DIMENSION d FROM t ON k (LEVEL l = c1) \
             MEASURE m ({written} ALONG d)"
        );
        let definition = created(&sql);
        assert_eq!(
            definition.measures[0].rule("d"),
            Some(expected),
            "`{written}` did not read as {expected:?}"
        );
    }
}

#[test]
fn a_parent_child_dimension_is_distinct_from_a_level_hierarchy() {
    // A ragged org chart has no fixed depth, and flattening it into levels is what forces
    // the padding FR-QUERY-11 forbids. The two must be separately expressible.
    let sql = "CREATE CUBE org FROM headcount \
               DIMENSION people FROM employees ON employee_id ( \
                   LEVEL person = employee_id, PARENT employee_id TO manager_id) \
               MEASURE headcount (SUM ALONG people)";
    let definition = created(sql);

    assert_eq!(
        definition.dimensions[0].parent_child,
        Some(("employee_id".to_string(), "manager_id".to_string()))
    );
}

#[test]
fn declared_rollup_edges_become_a_hierarchy() {
    let definition = created(FULL);
    let rollups = definition.dimensions[0]
        .rollups
        .as_ref()
        .expect("the geography dimension declared a rollup edge");

    assert!(rollups.members().contains("emea"));
    assert!(rollups.children_of("world").contains("emea"));
    assert!(
        definition.dimensions[1].rollups.is_none(),
        "a dimension that declared no edges gets no hierarchy, not an empty one"
    );
}

#[test]
fn a_cube_with_no_target_lag_is_declared_rather_than_maintained() {
    // Persisting a definition is cheap; materialising is storage and work. A cube must not
    // acquire either by being written down.
    let sql = "CREATE CUBE c FROM f DIMENSION d FROM t ON k (LEVEL l = c1) \
               MEASURE m (SUM ALONG d)";
    let definition = created(sql);

    assert_eq!(definition.target_lag, None);
    assert!(definition.pinned.is_empty());
}

#[test]
fn the_singular_version_reads_the_same_as_the_plural() {
    // `WITHIN 1 VERSIONS` reads badly enough that somebody will write the singular.
    let sql = "CREATE CUBE c FROM f DIMENSION d FROM t ON k (LEVEL l = c1) \
               MEASURE m (SUM ALONG d) MAINTAINED WITHIN 1 VERSION";
    assert_eq!(created(sql).target_lag, Some(1));
}

#[test]
fn more_than_one_shape_can_be_pinned() {
    let sql = "CREATE CUBE c FROM f \
               DIMENSION a FROM t1 ON k1 (LEVEL l = c1) \
               DIMENSION b FROM t2 ON k2 (LEVEL l = c2) \
               MEASURE m (SUM ALONG a, SUM ALONG b) \
               PINNED (a) PINNED (a, b)";
    let definition = created(sql);

    assert_eq!(
        definition.pinned,
        vec![vec!["a".to_string()], vec!["a".to_string(), "b".to_string()]]
    );
}

#[test]
fn keywords_are_case_insensitive_and_identifiers_are_not_folded() {
    // Two separate claims, and mixing them up is how `SELECT` becomes case-sensitive by
    // accident. A keyword is matched without regard to case; a name is kept exactly.
    let sql = "create CUBE Sales fRoM Orders \
               dimension Geo from Regions on Region_Id (level Country = Country_Code) \
               measure Amount (sum along Geo)";
    let definition = created(sql);

    assert_eq!(definition.name, "Sales");
    assert_eq!(definition.fact_table, "Orders");
    assert_eq!(definition.dimensions[0].name, "Geo");
    assert_eq!(definition.dimensions[0].levels[0].column, "Country_Code");
}

#[test]
fn a_keyword_may_be_used_as_a_name() {
    // Keywords are not reserved. Somebody has a column called `sum` and a dimension called
    // `level`, and every position that takes a name is one where a keyword cannot also
    // appear, so there is nothing to disambiguate and nothing to refuse.
    let sql = "CREATE CUBE cube FROM measure \
               DIMENSION level FROM from ON along (LEVEL sum = min) \
               MEASURE max (SUM ALONG level)";
    let definition = created(sql);

    assert_eq!(definition.name, "cube");
    assert_eq!(definition.fact_table, "measure");
    assert_eq!(definition.dimensions[0].name, "level");
    assert_eq!(definition.dimensions[0].levels[0].name, "sum");
    assert_eq!(definition.measures[0].name, "max");
}

#[test]
fn a_quoted_identifier_keeps_its_case_and_its_spaces() {
    let sql = "CREATE CUBE \"Sales By Region\" FROM \"Order Facts\" \
               DIMENSION g FROM t ON k (LEVEL l = \"Country Code\") \
               MEASURE m (SUM ALONG g)";
    let definition = created(sql);

    assert_eq!(definition.name, "Sales By Region");
    assert_eq!(definition.fact_table, "Order Facts");
    assert_eq!(definition.dimensions[0].levels[0].column, "Country Code");
}

#[test]
fn a_trailing_semicolon_is_accepted_and_nothing_else_is() {
    assert!(matches!(
        parse("DROP CUBE sales;"),
        Some(Ok(Statement::Drop { .. }))
    ));
    // Trailing tokens mean the writer intended something the grammar did not take. Dropping
    // them silently would build a cube subtly unlike the one that was asked for.
    let trailing = parse("DROP CUBE sales AND ALSO everything");
    assert!(
        matches!(&trailing, Some(Err(error)) if error.expected.contains("end of the statement")),
        "expected a complaint about trailing tokens, got {trailing:?}"
    );
}

#[test]
fn dropping_a_cube_is_read_with_and_without_if_exists() {
    match parse("DROP CUBE sales") {
        Some(Ok(Statement::Drop { name, if_exists, .. })) => {
            assert_eq!(name, "sales");
            assert!(!if_exists);
        }
        other => panic!("expected a DROP, got {other:?}"),
    }
    match parse("drop cube if exists sales") {
        Some(Ok(Statement::Drop { name, if_exists, .. })) => {
            assert_eq!(name, "sales");
            assert!(if_exists, "IF EXISTS is carried rather than resolved here");
        }
        other => panic!("expected a DROP, got {other:?}"),
    }
}

#[test]
fn anything_that_is_not_cube_ddl_is_left_entirely_alone() {
    // The load-bearing test. This parser runs before the engine, so every other statement in
    // the language has to pass through it untouched — including malformed ones, whose error
    // must come from the engine that owns the language rather than from a pre-filter that
    // happened to look first.
    for sql in [
        "SELECT 1",
        "SELECT * FROM cube_rollup('sales', 'amount', 'by=region')",
        "CREATE TABLE cube (id INT)",
        "CREATE TABLE cubes AS SELECT * FROM sales",
        "DROP TABLE sales",
        "SELECT cube, fact_table FROM cubes()",
        "CREATE",
        "DROP",
        "",
        "   ",
        ";",
        "-- CREATE CUBE sales FROM orders",
        "'CREATE CUBE'",
        "CREATECUBE sales FROM orders",
    ] {
        assert!(
            parse(sql).is_none(),
            "`{sql}` is not cube DDL and must reach the engine untouched"
        );
    }
}

#[test]
fn a_statement_that_opens_as_cube_ddl_and_then_does_not_parse_is_an_error_here() {
    // The other side of the previous test, and the reason it is not simply "return None on
    // anything unrecognised": `CREATE CUBE` followed by nonsense is not a query DataFusion
    // can make sense of either, so forwarding it would report the wrong error at the wrong
    // layer and name a token nobody wrote.
    for sql in [
        "CREATE CUBE",
        "CREATE CUBE sales",
        "CREATE CUBE sales FROM",
        "CREATE CUBE sales FROM orders",
        "CREATE CUBE sales FROM orders DIMENSION",
        "CREATE CUBE sales FROM orders MEASURE m (SUM ALONG d)",
        "DROP CUBE",
        "DROP CUBE IF sales",
    ] {
        assert!(
            matches!(parse(sql), Some(Err(_))),
            "`{sql}` opens as cube DDL and does not parse, so it is an error rather than a \
             statement to pass along"
        );
    }
}

#[test]
fn a_syntax_error_says_what_was_wanted_and_where() {
    // A cube definition is long enough that "unexpected token" without a position is a hunt.
    let sql = "CREATE CUBE sales FROM orders \
               DIMENSION geography FROM regions ON region_id (LEVEL country country_code) \
               MEASURE amount (SUM ALONG geography)";
    let Some(Err(error)) = parse(sql) else {
        panic!("a level with no `=` is a syntax error");
    };

    assert_eq!(error.expected, "`=`");
    assert_eq!(error.found.as_deref(), Some("country_code"));
    assert!(error.at > 0, "the offset locates the token rather than the statement");
    assert!(
        error.to_string().contains("offset"),
        "the message carries the position: {error}"
    );
}

#[test]
fn a_missing_clause_is_named_in_the_words_a_writer_would_use() {
    let sql = "CREATE CUBE sales FROM orders MEASURE amount (SUM ALONG geography)";
    let Some(Err(error)) = parse(sql) else {
        panic!("a cube with no dimensions does not parse");
    };
    assert!(
        error.expected.contains("DIMENSION"),
        "the message should show the clause that is missing: {error}"
    );

    let sql = "CREATE CUBE sales FROM orders DIMENSION g FROM t ON k (LEVEL l = c)";
    let Some(Err(error)) = parse(sql) else {
        panic!("a cube with no measures does not parse");
    };
    assert!(
        error.expected.contains("MEASURE"),
        "the message should show the clause that is missing: {error}"
    );
}

#[test]
fn validation_is_left_to_the_validator_rather_than_reimplemented() {
    // A measure missing a rule for a declared dimension is exactly the defect the measure
    // model exists to catch — and it is caught by `validate`, which reports *every*
    // rejection, not by this parser reporting the first. A second implementation of that
    // rule here would be a second implementation to disagree with the first.
    let sql = "CREATE CUBE c FROM f \
               DIMENSION a FROM t1 ON k1 (LEVEL l = c1) \
               DIMENSION b FROM t2 ON k2 (LEVEL l = c2) \
               MEASURE m (SUM ALONG a)";
    let definition = created(sql);

    assert_eq!(definition.measures[0].rule("b"), None, "the parser read what was written");
    let rejections = definition.validate().expect_err("and the validator refuses it");
    assert!(!rejections.is_empty());
}

#[test]
fn a_staleness_target_too_large_to_hold_is_refused_rather_than_folded() {
    // Saturating would accept this as some other number entirely, which is the shape of
    // mistake worth a message rather than a silent reinterpretation.
    let sql = "CREATE CUBE c FROM f DIMENSION d FROM t ON k (LEVEL l = c1) \
               MEASURE m (SUM ALONG d) MAINTAINED WITHIN 99999999999999999999999 VERSIONS";
    assert!(
        matches!(parse(sql), Some(Err(_))),
        "a number that does not fit is not silently some other number"
    );
}

#[test]
fn whitespace_and_line_breaks_do_not_change_what_was_read() {
    let compact = "CREATE CUBE c FROM f DIMENSION d FROM t ON k(LEVEL l=c1) \
                   MEASURE m(SUM ALONG d)";
    let spread = "CREATE   CUBE   c\n  FROM   f\n\n  DIMENSION d FROM t ON k (\n \
                  LEVEL l = c1\n  )\n  MEASURE m ( SUM ALONG d )\n";

    assert_eq!(created(compact), created(spread));
}

#[test]
fn a_cube_can_name_a_table_in_a_schema() {
    // The lexer stops an unquoted word at a `.`, so `sales.orders` arrived as three tokens and
    // the statement failed with *"expected at least one DIMENSION"* --- a message about the
    // wrong half of the statement, which is how this survived. The bare form was no better: it
    // resolved only while one schema claimed the name, and every warehouse with two schemas
    // that both hold an `orders` could not build a cube at all.
    let definition = created(
        "CREATE CUBE sales FROM sales.orders \
         DIMENSION geography FROM reference.regions ON region_id (LEVEL region = region_name) \
         MEASURE amount (SUM ALONG geography)",
    );

    assert_eq!(definition.fact_table, "sales.orders");
    assert_eq!(definition.dimensions[0].table, "reference.regions");
    definition.validate().expect("a qualified cube is still a usable cube");
}

#[test]
fn a_bare_name_is_still_a_name() {
    // The form every existing definition on disk uses. Qualifying had to be an addition rather
    // than a replacement, or reading a catalogue written last month would stop working.
    let definition = created(
        "CREATE CUBE sales FROM orders \
         DIMENSION geography FROM regions ON region_id (LEVEL region = region_name) \
         MEASURE amount (SUM ALONG geography)",
    );
    assert_eq!(definition.fact_table, "orders");
    assert_eq!(definition.dimensions[0].table, "regions");
}

#[test]
fn a_dot_where_a_table_name_does_not_belong_is_still_refused() {
    // Only the two table positions take a qualified name. A dimension name, a level name and a
    // column name are not qualified, and accepting a dot in one would parse a typo into a name
    // nothing resolves --- a definition that saves cleanly and hydrates to nothing.
    for sql in [
        "CREATE CUBE sales FROM orders \
         DIMENSION geo.graphy FROM regions ON region_id (LEVEL region = region_name) \
         MEASURE amount (SUM ALONG geography)",
        "CREATE CUBE sales FROM orders \
         DIMENSION geography FROM regions ON tbl.region_id (LEVEL region = region_name) \
         MEASURE amount (SUM ALONG geography)",
    ] {
        assert!(
            matches!(parse(sql), Some(Err(_))),
            "a dot outside a table position was accepted: {sql}"
        );
    }
}
