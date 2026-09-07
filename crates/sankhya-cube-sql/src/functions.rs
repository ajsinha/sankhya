//! The table functions themselves.
//!
//! Each resolves a named cube, applies any overlay, narrows and rolls up as the options
//! ask, and returns the rows with everything that qualifies the numbers attached to every
//! one of them.
//!
//! The common columns are the point. `completeness` and `withheld` make a partial
//! total impossible to project away accidentally; `overlay` names the scenario a what-if
//! came from; `definition_version` and `snapshot` let a cube figure be reconciled with a
//! relational one taken at a different moment; `materialised` and `from_cuboid` answer "why
//! was this fast or slow?". Every one of them would be tidier as query metadata, and every
//! one would then be lost by the first `SELECT` that did not mention it.
//!
//! `materialised` reports **what happened**. Until 2026-08-28 it reported the `materialise`
//! argument the caller had passed, which made it a mirror rather than a measurement --- and
//! it went unnoticed for as long as nothing served a cuboid, because the honest answer was
//! `false` for every query and the echo agreed with it by accident.

use crate::args::Arguments;
use crate::catalog::{CubeCatalog, Published};
use crate::result::CubeTable;
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use datafusion::catalog::{TableFunctionImpl, TableProvider};
use datafusion::common::{plan_datafusion_err, plan_err, Result};
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::Expr;
use sankhya_cube::cells::Cells;
use sankhya_cube::complete::{Assessed, Completeness, Threshold};
use sankhya_cube::navigate::{dice, roll_up, slice, Ordered};
use sankhya_cube::overlay::{Allocation, Applied};
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use std::sync::Arc;

/// Register every cube function against a session.
///
/// One call, so a session either has the whole surface or none of it. A partially
/// registered catalogue means a query works on one node and fails on another.
/// How a measure declared with `AGGREGATION <name>` is computed.
///
/// A callback rather than a dependency, so this crate knows nothing about processes,
/// interpreters or sandboxes --- it knows that some rules are computed by somebody else, and
/// what to hand them: the name they were declared under, and the contributions of one cell.
///
/// `None` on a server where none has been declared, which is the common case and costs nothing.
pub type Supplied = Arc<dyn Fn(&str, &[f64]) -> std::result::Result<f64, String> + Send + Sync>;

pub fn register(
    context: &SessionContext,
    catalog: Arc<CubeCatalog>,
    log: Arc<sankhya_cube::querylog::QueryLog>,
    supplied: Option<Supplied>,
) {
    context.register_udtf(
        "cube_rollup",
        Arc::new(RollUp(Arc::clone(&catalog), Arc::clone(&log), supplied.clone())),
    );
    context.register_udtf("cube_slice", Arc::new(Slice(catalog, log, supplied)));
}

/// Record the shape a query asked for, so selection has something to read.
///
/// The **shape**, and nothing else: which cube, and which dimensions were grouped by. There is
/// nowhere in this call to put a member, a predicate or a principal, which is deliberate ---
/// a query log is the kind of thing that quietly becomes a record of who asked what about
/// whom, and this one records a list of column names anybody who may read the cube can
/// already get from `cube_dimensions`.
fn note_the_shape(log: &sankhya_cube::querylog::QueryLog, cube: &str, args: &Arguments) {
    let by = args.list("by");
    let asked: Vec<&str> = by.iter().map(String::as_str).collect();
    log.record(cube, sankhya_cube::algo::Cuboid::of(&asked));
}

/// Refuse a `materialise` option this system does not understand.
///
/// # Why this is validated here and decided elsewhere
///
/// The option is read **twice**, and that is a consequence of the architecture rather than an
/// oversight worth hiding. Whether to serve a query from a cuboid has to be decided before
/// this function runs --- the server publishes cells into the catalogue while registering
/// them, and by the time `call` happens the choice is already made --- so the server scans
/// the statement text for it. That scan cannot refuse anything: it runs before planning.
///
/// This is where a refusal is possible, so this is where the value is checked. Without it
/// `materialise=pinnd` would pass the known-option check in `args.rs`, be silently ignored by
/// the server's scan, take its default, and produce a result wrong in a way the query text
/// does not reveal --- which is the precise failure `args.rs` refuses unknown options to
/// prevent, arriving through the value instead of the name.
fn check_materialise(args: &Arguments) -> Result<()> {
    let Some(asked) = args.string("materialise") else {
        return Ok(());
    };
    match asked.to_lowercase().as_str() {
        "true" | "yes" | "on" | "false" | "no" | "off" | "pinned" => Ok(()),
        other => plan_err!(
            "the 'materialise' option must be true, false or pinned, and '{other}' is not. \
             It narrows what this query will use: 'false' computes from the base data, \
             'pinned' uses only cuboids the definition pins. There is no value that widens \
             it --- a session that could spend more of an operator's storage would be a \
             storage grant to anybody who can open one"
        ),
    }
}

/// The columns every cube function carries, whatever else it returns.
fn provenance_fields() -> Vec<Field> {
    vec![
        Field::new("definition_version", DataType::UInt64, false),
        Field::new("snapshot", DataType::UInt64, false),
        Field::new("materialised", DataType::Boolean, false),
        Field::new("from_cuboid", DataType::Utf8, false),
        // Nullable: an aggregate over no rows has *no* completeness, which is not the same
        // as complete. See `sankhya_cube::complete`.
        Field::new("completeness", DataType::Float64, true),
        Field::new("withheld", DataType::UInt64, false),
        // Null means published data. A name means a what-if, and which one.
        Field::new("overlay", DataType::Utf8, true),
    ]
}

/// The provenance columns, filled for `rows` rows.
fn provenance_columns(
    published: &Published,
    completeness: &Completeness,
    overlay: Option<&str>,
    from: &str,
    materialised: bool,
    rows: usize,
) -> Vec<ArrayRef> {
    vec![
        Arc::new(UInt64Array::from(vec![published.cube.version(); rows])),
        Arc::new(UInt64Array::from(vec![published.snapshot; rows])),
        Arc::new(BooleanArray::from(vec![materialised; rows])),
        Arc::new(StringArray::from(vec![from; rows])),
        Arc::new(Float64Array::from(vec![completeness.fraction(); rows])),
        Arc::new(UInt64Array::from(vec![completeness.withheld(); rows])),
        Arc::new(StringArray::from(vec![overlay; rows])),
    ]
}

/// Resolve the cells for the cube named in argument one and the measure in argument two.
///
/// Both, together, because a set of cells holds one measure's values --- so the pair is the
/// identity of what a query is asking for, and resolving on the cube alone gives whichever
/// measure happened to be published last.
fn cube_of(catalog: &CubeCatalog, args: &Arguments) -> Result<Published> {
    let name = args.string_at(0, "cube name")?;
    let measure = args.string_at(1, "measure name")?;
    catalog
        .resolve(&name, &measure)
        .map_err(|e| plan_datafusion_err!("{e}"))
}

/// Resolve the measure named in argument two against the cube.
///
/// An unknown measure is refused rather than defaulted. There is no sensible default: the
/// measure decides which roll-ups are legal, so guessing one produces an answer computed
/// under rules nobody chose.
fn measure_of(published: &Published, args: &Arguments) -> Result<Measure> {
    let name = args.string_at(1, "measure name")?;
    // Belt and braces. `cube_of` resolved the cells *by* this measure, so a mismatch here
    // would mean the catalogue filed cells under a name that is not their own --- which
    // `CubeCatalog::publish` makes impossible by taking the name from the cells. Asserted
    // anyway, because the failure it guards is a plausible number rather than an error, and
    // that is worth one comparison.
    debug_assert_eq!(published.measure, name, "cells filed under another measure's name");
    published.cube.measure(&name).cloned().ok_or_else(|| {
        let known: Vec<&str> = published.cube.measures().iter().map(|m| m.name.as_str()).collect();
        plan_datafusion_err!(
            "cube '{}' has no measure named '{}' — it has {:?}",
            published.cube.name(),
            name,
            known
        )
    })
}

/// Apply the overlay the options name, if any.
fn overlaid(
    catalog: &CubeCatalog,
    published: &Published,
    args: &Arguments,
) -> Result<Applied<Cells>> {
    let Some(name) = args.string("overlay") else {
        return Ok(Applied::published((*published.cells).clone()));
    };
    let overlay = catalog
        .overlay(&name)
        .map_err(|e| plan_datafusion_err!("{e}"))?;
    let allocation = match args.string("allocate").as_deref() {
        None | Some("refuse") => Allocation::Refuse,
        Some("pro_rata") => Allocation::ProRata,
        Some(other) => {
            return plan_err!(
                "'{other}' is not an allocation. Write 'allocate=pro_rata' to spread an \
                 edited total across the cells beneath it, or leave it out to be refused a \
                 grain the overlay does not contain"
            )
        }
    };
    overlay
        .apply(&published.cells, published.cube.version(), allocation)
        .map_err(|e| plan_datafusion_err!("{e}"))
}

/// Narrow by the `where` option, written as `dimension:member|member`.
fn narrowed(cells: &Cells, args: &Arguments) -> Result<Cells> {
    let restrictions = args.list("where");
    if restrictions.is_empty() {
        return Ok(cells.clone());
    }
    let mut parsed: Vec<(String, Vec<String>)> = Vec::new();
    for restriction in &restrictions {
        let Some((dimension, members)) = restriction.split_once(':') else {
            return plan_err!(
                "'{restriction}' is not a restriction. Write them as \
                 'where=region:north/south|period:jan', dimension first"
            );
        };
        parsed.push((
            dimension.trim().to_string(),
            members.split('/').map(|m| m.trim().to_string()).collect(),
        ));
    }

    let borrowed: Vec<(&str, Vec<&str>)> = parsed
        .iter()
        .map(|(d, m)| (d.as_str(), m.iter().map(String::as_str).collect()))
        .collect();
    let pairs: Vec<(&str, &[&str])> = borrowed
        .iter()
        .map(|(d, m)| (*d, m.as_slice()))
        .collect();

    let diced = dice(cells, &pairs);
    if !diced.ignored.is_empty() {
        // A filter that silently did not apply is found in a reconciliation months later.
        return plan_err!(
            "cube '{}' has no dimension(s) {:?}, named in the 'where' option. Refused \
             rather than ignored: a restriction that did not apply returns more rows than \
             the query asked for, and nothing says so",
            "this cube",
            diced.ignored
        );
    }
    Ok(diced.cells)
}

/// Roll up to the dimensions the `by` option names, dropping the rest.
///
/// # A name that matches nothing rolls everything away
///
/// The `by` **value** was never checked against the cube's dimensions, and the comparison was
/// case-sensitive while every keyword in this file is not. So `by=regoin` --- or `by=Region` on
/// a cube that spells it `region` --- kept no dimension at all, rolled the axis away, and
/// returned a subtotal labelled as a breakdown. `CLI-09`.
///
/// `where` was already checked, in the function directly above, under a comment reading *"a
/// filter that silently did not apply is found in a reconciliation months later"*. The same
/// sentence is true of a grain, and more so: a filter that did not apply returns too many rows,
/// which somebody may notice, while a grain that collapsed returns one row that looks like an
/// answer.
///
/// The test named `a_misspelled_option_is_refused_rather_than_taking_its_default` carried
/// `by=regoin` in a comment and then tested a misspelled **key**.
fn rolled(cells: &Cells, measure: &Measure, args: &Arguments) -> Result<Cells> {
    let asked = args.list("by");
    let order = args.list("order");
    let stated: Vec<&str> = order.iter().map(String::as_str).collect();

    // Resolved case-insensitively, and refused when it resolves to nothing.
    let available = cells.dimensions();
    let mut keep: Vec<String> = Vec::with_capacity(asked.len());
    let mut unknown: Vec<String> = Vec::new();
    for wanted in &asked {
        match available
            .iter()
            .find(|dimension| dimension.eq_ignore_ascii_case(wanted))
        {
            Some(dimension) => keep.push(dimension.clone()),
            None => unknown.push(wanted.clone()),
        }
    }
    if !unknown.is_empty() {
        return plan_err!(
            "cube has no dimension(s) {unknown:?}, named in the 'by' option. Refused rather \
             than ignored: a grain that did not apply rolls the axis away and returns a \
             subtotal labelled as a breakdown, which reads as an answer. It has {available:?}"
        );
    }

    let mut out = cells.clone();
    let dropping: Vec<String> = out
        .dimensions()
        .iter()
        .filter(|d| !keep.contains(d))
        .cloned()
        .collect();
    for dimension in dropping {
        let ordered = if stated.is_empty() {
            Ordered::Unstated
        } else {
            Ordered::By(&stated)
        };
        out = roll_up(&out, &dimension, measure, ordered)
            .map_err(|e| plan_datafusion_err!("{e}"))?;
    }
    Ok(out)
}

/// Build the output schema: one column per dimension, the measure, then provenance.
fn schema_for(dimensions: &[String], measure: &str) -> SchemaRef {
    let mut fields: Vec<Field> = dimensions
        .iter()
        .map(|d| Field::new(d, DataType::Utf8, false))
        .collect();
    // Nullable: an absent cell is absent, not zero. See `sankhya_cube::cells`.
    fields.push(Field::new(measure, DataType::Float64, true));
    fields.extend(provenance_fields());
    Arc::new(Schema::new(fields))
}

/// How the contributions inside one cell combine, for this measure at this grain.
///
/// # Which of a measure's rules applies
///
/// A measure declares a rule **per dimension**. The contributions sitting in one cell are the
/// facts that were aggregated *along the dimensions rolled away* to reach this grain, so the
/// rule that applies is the rule along those.
///
/// At the base grain nothing has been rolled away and a cell holds facts that shared an
/// address, so there is no rolled-away dimension to take a rule from. The measure's own rules
/// are used instead: they are what it says about combining its own values anywhere.
///
/// # Why disagreement is refused rather than resolved
///
/// Two dimensions rolled away with different rules --- `Sum` along one and `Last` along another
/// --- have no single answer, and the answer depends on the order the planner happened to
/// choose. Picking one produces a number that changes between runs and looks plausible every
/// time, which is the failure mode with no symptom. So it is refused, naming both.
///
/// # Errors
///
/// When the rules that apply disagree.
pub fn reduction_for(measure: &Measure, kept: &[String]) -> Result<Rule> {
    let applying: Vec<&Along> = measure
        .rules
        .iter()
        .filter(|along| !kept.contains(&along.dimension))
        .collect();
    // Nothing rolled away: the measure's own rules are what it says about its values.
    let applying: Vec<&Along> = if applying.is_empty() {
        measure.rules.iter().collect()
    } else {
        applying
    };

    // Whether anything was actually rolled away, which decides what the refusal below may
    // truthfully say. At the base grain the fallback above substituted the measure's own
    // rules, and the message then told a caller their measure "combines by sum and last along
    // the dimensions being rolled away" for a query that rolls nothing away at all --- the
    // finest grain the cube has. A refusal that misdescribes what it refused sends the reader
    // to look at the wrong half of their model.
    let rolling_away = measure.rules.iter().any(|along| !kept.contains(&along.dimension));
    let mut distinct: Vec<Rule> = Vec::new();
    for along in &applying {
        if !distinct.contains(&along.rule) {
            distinct.push(along.rule);
        }
    }
    match distinct.as_slice() {
        // A measure with no declared rules at all cannot be reduced by guessing.
        [] => plan_err!(
            "the measure `{}` declares no rule, so there is no way to combine the facts in a \
             cell. A measure must say how it composes along every dimension",
            measure.name
        ),
        [only] => Ok(*only),
        several => {
            let rules = several
                .iter()
                .map(|rule| rule.as_str())
                .collect::<Vec<&str>>()
                .join(" and ");
            if rolling_away {
                plan_err!(
                    "the measure `{}` combines by {rules} along the dimensions being rolled \
                     away, and those disagree. Refused rather than resolved: the answer would \
                     depend on the order the planner chose, and would look plausible whichever \
                     it picked",
                    measure.name
                )
            } else {
                plan_err!(
                    "this query keeps every dimension, so nothing is rolled away --- but two \
                     facts can still share one address, and the measure `{}` combines by \
                     {rules} depending on which dimension you ask about, so there is no single \
                     way to fold them. Refused rather than resolved: the answer would depend on \
                     the order the planner chose. Roll up along one dimension, or declare one \
                     rule for this measure",
                    measure.name
                )
            }
        }
    }
}

/// Turn a cube into rows.
fn batch(
    published: &Published,
    cells: &Cells,
    measure: &Measure,
    overlay: Option<&str>,
    completeness: &Completeness,
    materialised: bool,
    supplied: Option<&Supplied>,
) -> Result<Arc<dyn TableProvider>> {
    let dimensions: Vec<String> = cells.dimensions().to_vec();
    let schema = schema_for(&dimensions, &measure.name);

    // The rule the **measure declares**, not `Rule::Sum`.
    //
    // This read every cell with a hardcoded sum, so `MEASURE amount (MEAN ALONG region)`
    // answered with the total and `MAX ALONG region` answered with the total. The
    // composability half of the model was enforced correctly --- a measure that cannot be
    // rolled up is still refused --- and the **value never saw the rule**, which is precisely
    // the failure the whole additivity model exists to prevent. A number of the right
    // magnitude, the right sign, and no meaning.
    //
    // Found by an adversarial review on 2026-09-01: a group whose mean is 186.75 and whose
    // max is 373.5 answered 15,687 for both.
    let rule = reduction_for(measure, &dimensions)?;
    // A rule the user wrote is computed by whoever owns the worker, over the contributions of
    // each cell. `Cells::get` answers `None` for it deliberately --- that type cannot run
    // somebody's Python, and the layer that can is above it.
    let rows: Vec<(&Vec<String>, Option<f64>)> = if let Rule::Supplied { .. } = rule {
        let name = supplied_name(measure, &dimensions);
        let Some((name, compute)) = name.zip(supplied) else {
            return plan_err!(
                "the measure `{}` is computed by an aggregation of your own, and this server \
                 has none by that name. `SHOW AGGREGATIONS` lists what it has; the cube was \
                 declared against one that has since been dropped, or was never declared here",
                measure.name
            );
        };
        let mut rows = Vec::new();
        for address in cells.addresses() {
            let contributions = cells.contributions_at(address).unwrap_or(&[]);
            if contributions.is_empty() {
                rows.push((address, None));
                continue;
            }
            match compute(&name, contributions) {
                Ok(value) => rows.push((address, Some(value))),
                Err(said) => return plan_err!("{}: {said}", measure.name),
            }
        }
        rows
    } else {
        cells
            .addresses()
            .map(|address| (address, cells.get(address, rule)))
            .collect()
    };

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    for axis in 0..dimensions.len() {
        let members: Vec<&str> = rows
            .iter()
            .map(|(address, _)| address.get(axis).map_or("", String::as_str))
            .collect();
        columns.push(Arc::new(StringArray::from(members)));
    }
    let values: Vec<Option<f64>> = rows.iter().map(|(_, value)| *value).collect();
    columns.push(Arc::new(Float64Array::from(values)));

    let from = if dimensions.is_empty() {
        "()".to_string()
    } else {
        dimensions.join(", ")
    };
    columns.extend(provenance_columns(
        published,
        completeness,
        overlay,
        &from,
        materialised,
        rows.len(),
    ));

    let batch = RecordBatch::try_new(Arc::clone(&schema), columns)
        .map_err(|e| plan_datafusion_err!("building the cube result: {e}"))?;
    Ok(Arc::new(CubeTable::new(schema, batch)))
}

/// The aggregation a measure names along the dimensions rolled away.
///
/// The same question `reduction_for` answers, asked of the *name* rather than the rule. Kept
/// beside it rather than folded into it because `Rule` is `Copy` and holds no string --- see
/// `Rule::Supplied`.
fn supplied_name(measure: &Measure, kept: &[String]) -> Option<String> {
    let rolled: Vec<&Along> = measure
        .rules
        .iter()
        .filter(|along| !kept.contains(&along.dimension))
        .collect();
    let candidates = if rolled.is_empty() { measure.rules.iter().collect() } else { rolled };
    candidates.iter().find_map(|along| along.supplied.clone())
}

/// Refuse the whole result when it saw too little of its input.
///
/// `FR-QUERY-13`, at the surface a caller actually uses. The threshold is a per-query
/// option because how much filtering is acceptable is a property of the question, not of
/// the cube.
fn check_completeness(completeness: &Completeness, args: &Arguments) -> Result<()> {
    let Some(required) = args.number("min_completeness")? else {
        return Ok(());
    };
    let threshold = Threshold::at_least(required).map_err(|e| plan_datafusion_err!("{e}"))?;
    Assessed::new((), *completeness)
        .meeting(&threshold)
        .map_err(|e| plan_datafusion_err!("{e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------

/// `cube_rollup(cube, measure, options)` --- a breakdown at the grain `by` names.
/// `Debug` by hand: a callback has no useful debug form, and deriving it would make this
/// struct undebuggable rather than making the callback printable.
struct RollUp(Arc<CubeCatalog>, Arc<sankhya_cube::querylog::QueryLog>, Option<Supplied>);

impl std::fmt::Debug for RollUp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RollUp").field("supplied", &self.2.is_some()).finish()
    }
}

impl TableFunctionImpl for RollUp {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let published = cube_of(&self.0, &args)?;
        let measure = measure_of(&published, &args)?;
        note_the_shape(&self.1, published.cube.name(), &args);

        let applied = overlaid(&self.0, &published, &args)?;
        let overlay = applied.overlay().map(str::to_string);
        let narrowed = narrowed(applied.regardless(), &args)?;

        // From hydration, not from the rows that survived. Dicing is a query narrowing
        // rather than a loss, so it does not change what fraction of the fact table this
        // cube saw.
        let completeness = published.completeness;
        check_completeness(&completeness, &args)?;

        let rolled = rolled(&narrowed, &measure, &args)?;
        // **What happened, not what was asked for.** This column used to be
        // `args.boolean("materialise")` --- the caller's own argument, echoed back --- so an
        // operator asking "why was this fast?" was told whatever their query had typed.
        check_materialise(&args)?;
        let materialised = published.from_cuboid;
        batch(
            &published,
            &rolled,
            &measure,
            overlay.as_deref(),
            &completeness,
            materialised,
            self.2.as_ref(),
        )
    }
}

/// `cube_slice(cube, measure, options)` --- one member fixed, that axis dropped.
struct Slice(Arc<CubeCatalog>, Arc<sankhya_cube::querylog::QueryLog>, Option<Supplied>);

impl std::fmt::Debug for Slice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slice").field("supplied", &self.2.is_some()).finish()
    }
}

impl TableFunctionImpl for Slice {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let published = cube_of(&self.0, &args)?;
        let measure = measure_of(&published, &args)?;
        note_the_shape(&self.1, published.cube.name(), &args);

        let Some(restriction) = args.string("where") else {
            return plan_err!(
                "cube_slice needs the member to slice to, as 'where=region:north'"
            );
        };
        let Some((dimension, member)) = restriction.split_once(':') else {
            return plan_err!("'{restriction}' is not a slice. Write it as 'where=region:north'");
        };
        if published
            .cells
            .axis(dimension.trim())
            .is_none()
        {
            return plan_err!(
                "'{}' is not a dimension of this cube. Slicing on one it does not have \
                 would return every row unchanged, which reads as a filter that matched \
                 everything",
                dimension.trim()
            );
        }

        let applied = overlaid(&self.0, &published, &args)?;
        let overlay = applied.overlay().map(str::to_string);
        let sliced = slice(applied.regardless(), dimension.trim(), member.trim());

        let completeness = published.completeness;
        check_completeness(&completeness, &args)?;

        // **What happened, not a constant.** This was the literal `false`, so a slice served
        // from a cuboid reported that it was not --- the same echo-your-own-input defect
        // `RollUp` above had fixed, surviving in the other navigation because only one of the
        // two was repaired.
        let materialised = published.from_cuboid;
        batch(
            &published,
            &sliced,
            &measure,
            overlay.as_deref(),
            &completeness,
            materialised,
            self.2.as_ref(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Tests may panic --- that is how a test reports a failure. The workspace denies `unwrap`,
    // `expect` and `panic` because a *server* must not do those things on data it did not
    // choose; a test chooses all of its data, and an assertion that cannot fail loudly is
    // worse than useless.
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::reduction_for;
    use sankhya_cube_algo::measure::{Along, Measure, Rule};

    fn measure(rules: &[(&str, Rule)]) -> Measure {
        Measure::new(
            "amount",
            rules.iter().map(|(d, r)| Along::new(*d, *r)).collect(),
        )
    }

    #[test]
    fn a_rolled_away_dimension_decides_how_a_cell_reduces() {
        // The defect this exists for: every cell was read with a hardcoded `Rule::Sum`, so a
        // measure declared `MEAN ALONG region` answered with the total. The composability half
        // of the model worked and the **value never saw the rule**.
        let m = measure(&[("region", Rule::Max), ("period", Rule::Max)]);
        // `region` is kept, `period` is rolled away, so `period`'s rule applies.
        assert_eq!(
            reduction_for(&m, &["region".to_string()]).expect("a rule"),
            Rule::Max
        );

        let m = measure(&[("region", Rule::Min), ("period", Rule::Min)]);
        assert_eq!(
            reduction_for(&m, &["region".to_string()]).expect("a rule"),
            Rule::Min
        );
    }

    #[test]
    fn the_rule_of_the_dimension_being_rolled_away_is_the_one_that_applies() {
        // Not the kept dimension's. A balance is additive across accounts and `Last` over
        // time: rolling *time* away must take the last value, and rolling *accounts* away must
        // sum -- from the same measure, in the same cube.
        let balance = measure(&[("account", Rule::Sum), ("month", Rule::Last)]);

        assert_eq!(
            reduction_for(&balance, &["account".to_string()]).expect("a rule"),
            Rule::Last,
            "rolling months away takes the last balance, never their sum"
        );
        assert_eq!(
            reduction_for(&balance, &["month".to_string()]).expect("a rule"),
            Rule::Sum,
            "rolling accounts away sums them"
        );
    }

    #[test]
    fn rules_that_disagree_are_refused_rather_than_resolved() {
        // Two dimensions rolled away with different rules have no single answer, and the one
        // produced would depend on the order the planner happened to choose -- so it would
        // change between runs and look plausible every time. That is the failure mode with no
        // symptom, and it is refused instead.
        let balance = measure(&[("account", Rule::Sum), ("month", Rule::Last)]);

        let refused = reduction_for(&balance, &[]).expect_err("ambiguous");
        let said = refused.to_string();
        assert!(said.contains("sum"), "{said}");
        assert!(said.contains("last"), "{said}");
        assert!(said.contains("disagree"), "{said}");
    }

    #[test]
    fn a_measure_with_no_rules_is_refused_rather_than_summed() {
        // A measure that says nothing about how it composes cannot be reduced by guessing,
        // and `Sum` is the guess that looks most like an answer.
        let nothing = Measure::new("amount", Vec::new());
        assert!(reduction_for(&nothing, &[]).is_err());
    }

    #[test]
    fn every_dimension_kept_falls_back_to_the_measures_own_rules() {
        // At the base grain nothing has been rolled away, and a cell still holds the facts
        // that shared an address. The measure's own rules are what it says about combining
        // its values anywhere.
        let m = measure(&[("region", Rule::Max), ("period", Rule::Max)]);
        assert_eq!(
            reduction_for(&m, &["region".to_string(), "period".to_string()]).expect("a rule"),
            Rule::Max
        );
    }
}
