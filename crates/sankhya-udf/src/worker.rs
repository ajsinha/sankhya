//! Running one, and deciding whether to trust it.

use crate::protocol::{
    self, Refused, ACCUMULATE, DESCRIBE, FINISH, IS_NUMBER, IS_STATE, MERGE,
};
use sankhya_sandbox::{Bounds, Outcome, Ready};
use std::path::{Path, PathBuf};

/// The worker SANKHYA ships, written to disk so the sandbox can bind it.
const HARNESS: &str = include_str!("harness.py");

/// An aggregation somebody declared, after it was exercised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregation {
    /// What it is called in a `MEASURE` clause.
    pub name: String,
    /// The author's Python, kept as written.
    ///
    /// Kept because `ADR-0023` Decision 4 makes creating one a **grant**, and a grant nobody
    /// can review is a grant nobody should give. It is also what a later reader needs in order
    /// to know what the column they are looking at was computed by.
    pub source: String,
    /// Whether it declared a `merge`, and therefore whether it composes.
    ///
    /// `ADR-0010`: *a declared `merge` means the measure composes --- it may be rolled up,
    /// answered from a materialised ancestor, and combined across partitions.* No `merge` means
    /// it is usable and computed from base data every time.
    pub composes: bool,
}

/// Runs user-supplied aggregations behind the boundary.
#[derive(Debug)]
pub struct Worker {
    ready: Ready,
    /// Where the harness and the interpreter live, kept alive for the worker's lifetime.
    home: tempfile::TempDir,
    python: PathBuf,
    readable: Vec<PathBuf>,
    bounds: Bounds,
}

impl Worker {
    /// Start one, or say why this machine cannot.
    ///
    /// # Errors
    ///
    /// [`Refused::NoBoundary`] when the sandbox cannot be built, or when there is no
    /// interpreter to run behind it. Both are refusals rather than a fallback: `ADR-0023`
    /// Decision 3 says that where the mechanism does not exist the feature is off, not
    /// degraded.
    pub fn start(python: &Path) -> Result<Self, Refused> {
        let ready = sankhya_sandbox::probe()
            .map_err(|unavailable| Refused::NoBoundary(unavailable.to_string()))?;
        if !python.exists() {
            return Err(Refused::NoBoundary(format!(
                "a user-supplied aggregation is written in Python and there is no interpreter \
                 at `{}`. Refused rather than answered another way: an aggregation that \
                 silently became a built-in would be a different number",
                python.display()
            )));
        }
        let home = tempfile::tempdir().map_err(|error| {
            Refused::NoBoundary(format!("the worker needs a directory and has none: {error}"))
        })?;
        std::fs::write(home.path().join("harness.py"), HARNESS).map_err(|error| {
            Refused::NoBoundary(format!("the worker could not be written: {error}"))
        })?;

        // The interpreter's own installation, asked of the interpreter rather than guessed.
        // Binding `/usr` wholesale would work and would put a great deal in the jail that has
        // nothing to do with running Python.
        let readable = interpreter_needs(python);

        Ok(Self {
            ready,
            home,
            python: python.to_path_buf(),
            readable,
            bounds: Bounds::modest(),
        })
    }

    /// Hold every call to different bounds.
    #[must_use]
    pub fn within(mut self, bounds: Bounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Fold a batch of values into a state.
    ///
    /// # Errors
    ///
    /// [`Refused`] when the function raised, ran past a bound, or answered unreadably.
    pub fn accumulate(
        &self,
        aggregation: &Aggregation,
        state: &[u8],
        values: &[f64],
    ) -> Result<Vec<u8>, Refused> {
        let request = protocol::request(
            ACCUMULATE,
            [aggregation.source.as_bytes(), state, &protocol::values(values), &[]],
        );
        let (kind, payload) = self.ask(&request)?;
        expect(kind, IS_STATE, payload)
    }

    /// Combine two partial states.
    ///
    /// # Errors
    ///
    /// [`Refused::Function`] when the aggregation declares no `merge`, which is not a failure
    /// but a fact about it: it does not compose, and the caller must go to base data.
    pub fn merge(
        &self,
        aggregation: &Aggregation,
        left: &[u8],
        right: &[u8],
    ) -> Result<Vec<u8>, Refused> {
        let request = protocol::request(MERGE, [aggregation.source.as_bytes(), left, right, &[]]);
        let (kind, payload) = self.ask(&request)?;
        expect(kind, IS_STATE, payload)
    }

    /// The state as the number a query returns.
    ///
    /// # Errors
    ///
    /// [`Refused`] as above.
    pub fn finish(&self, aggregation: &Aggregation, state: &[u8]) -> Result<f64, Refused> {
        let request = protocol::request(FINISH, [aggregation.source.as_bytes(), state, &[], &[]]);
        let (kind, payload) = self.ask(&request)?;
        let bytes = expect(kind, IS_NUMBER, payload)?;
        let eight: [u8; 8] = bytes
            .get(..8)
            .and_then(|slice| slice.try_into().ok())
            .ok_or_else(|| Refused::Protocol("a number that is not eight bytes".to_owned()))?;
        Ok(f64::from_le_bytes(eight))
    }

    /// Declare one: find out what it offers, then check that it agrees with itself.
    ///
    /// # Errors
    ///
    /// [`Refused::Function`] naming what disagreed, with both answers. `ADR-0010`: *a declared
    /// aggregation is exercised before it is trusted.*
    pub fn declare(&self, name: &str, source: &str) -> Result<Aggregation, Refused> {
        let described = self.ask(&protocol::request(
            DESCRIBE,
            [source.as_bytes(), &[], &[], &[]],
        ))?;
        let offered = String::from_utf8_lossy(&described.1).into_owned();
        let has = |method: &str| offered.contains(&format!("\"{method}\": true"));

        if !has("accumulate") || !has("finish") {
            return Err(Refused::Function(
                "an aggregation must define `accumulate(state, values)` and `finish(state)`. \
                 `merge(a, b)` is optional and is what decides whether it may be rolled up"
                    .to_owned(),
            ));
        }

        let candidate =
            Aggregation { name: name.to_owned(), source: source.to_owned(), composes: has("merge") };
        self.exercise(&candidate)?;
        Ok(candidate)
    }

    /// The determinism check, run before the aggregation is trusted with anything.
    ///
    /// Four answers over the same numbers, and they must be **bit for bit** the same:
    ///
    /// 1. one batch,
    /// 2. three batches, one after another,
    /// 3. three separate states merged left to right,
    /// 4. the same three merged right to left.
    ///
    /// One and two catch a function that depends on batch boundaries. Three and four catch one
    /// whose merge is not associative --- and it is the roll-up that would expose that, months
    /// later, as a total that does not match its parts.
    fn exercise(&self, aggregation: &Aggregation) -> Result<(), Refused> {
        // Deliberately awkward numbers: different magnitudes, a negative, a zero, and a value
        // whose sum depends on the order it is added in.
        let all: Vec<f64> = vec![
            1.0, 1e16, -1e16, 3.5, 0.0, -2.25, 7.125, 1e-9, 4.0, 100.0, -0.5, 9.75,
        ];
        let thirds: Vec<&[f64]> = all.chunks(4).collect();

        let whole = self.finish(aggregation, &self.accumulate(aggregation, &[], &all)?)?;

        let mut running = Vec::new();
        for chunk in &thirds {
            running = self.accumulate(aggregation, &running, chunk)?;
        }
        let in_pieces = self.finish(aggregation, &running)?;
        if !same(whole, in_pieces) {
            return Err(Refused::Function(format!(
                "`{}` gives a different answer depending on how the rows were batched: {whole} \
                 in one batch, {in_pieces} in three. A cuboid is built batch by batch and a \
                 query reads them whole, so the two would disagree by however much the \
                 batching happened to differ",
                aggregation.name
            )));
        }

        if !aggregation.composes {
            return Ok(());
        }

        let mut parts = Vec::new();
        for chunk in &thirds {
            parts.push(self.accumulate(aggregation, &[], chunk)?);
        }
        let (first, second, third) = (
            parts.first().cloned().unwrap_or_default(),
            parts.get(1).cloned().unwrap_or_default(),
            parts.get(2).cloned().unwrap_or_default(),
        );
        let leftwards = self.merge(aggregation, &self.merge(aggregation, &first, &second)?, &third)?;
        let rightwards = self.merge(aggregation, &first, &self.merge(aggregation, &second, &third)?)?;
        let left = self.finish(aggregation, &leftwards)?;
        let right = self.finish(aggregation, &rightwards)?;

        if !same(whole, left) || !same(left, right) {
            return Err(Refused::Function(format!(
                "`{}` declares a `merge`, and the merge is not associative: computed in one \
                 pass it gives {whole}; merged left to right, {left}; merged right to left, \
                 {right}. A declared merge is a claim that partial results compose, and a \
                 roll-up would combine them in whichever order the lattice reached them",
                aggregation.name
            )));
        }
        Ok(())
    }

    /// One round trip through the boundary.
    fn ask(&self, request: &[u8]) -> Result<(u16, Vec<u8>), Refused> {
        let harness = self.home.path().join("harness.py");
        let harness = harness.to_string_lossy().into_owned();
        let mut readable: Vec<&Path> = self.readable.iter().map(PathBuf::as_path).collect();
        readable.push(self.home.path());

        let outcome = self
            .ready
            .run(&self.python, &["-I", &harness], &readable, request, &self.bounds)
            .map_err(|error| {
                Refused::NoBoundary(format!("the worker could not be started: {error}"))
            })?;

        match outcome {
            Outcome::Answered(bytes) => protocol::response(&bytes),
            Outcome::OutOfTime { after } => Err(Refused::Bound(format!(
                "this aggregation did not finish within {after:?} and was stopped. An \
                 aggregation that never returns is an outage rather than an error, so it is \
                 killed rather than waited for"
            ))),
            Outcome::OutOfRoom { bound } => Err(Refused::Bound(format!(
                "this aggregation went past {bound}"
            ))),
            Outcome::Failed { code, signal, said } => Err(Refused::Function(format!(
                "the worker ended without answering (exit {code:?}, signal {signal:?}): {said}"
            ))),
        }
    }
}

/// The answer, if it is the kind that was expected.
fn expect(kind: u16, wanted: u16, payload: Vec<u8>) -> Result<Vec<u8>, Refused> {
    if kind == wanted {
        Ok(payload)
    } else {
        Err(Refused::Protocol(format!("it answered a {kind} where a {wanted} was expected")))
    }
}

/// Bit for bit, with `NaN` equal to itself.
///
/// Not "close enough". A difference of `1e-16` between a cuboid and the base data is exactly
/// what a figure failing to tie out looks like, and it is exactly what a function accumulating
/// in a different order produces.
fn same(left: f64, right: f64) -> bool {
    left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan())
}

/// What the interpreter needs in order to start, asked of the interpreter.
///
/// Guessing `/usr` would work and would put the whole of a machine's software in the jail. This
/// asks for the paths Python itself says it loads from, and adds the directories a dynamic
/// linker looks in --- which Python cannot report because it is not Python that reads them.
fn interpreter_needs(python: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let said = std::process::Command::new(python)
        .args([
            "-c",
            "import sys,os;print('\\n'.join([sys.executable, sys.base_prefix] + \
             [p for p in sys.path if p and os.path.isdir(p)]))",
        ])
        .output();
    if let Ok(said) = said {
        for line in String::from_utf8_lossy(&said.stdout).lines() {
            let path = PathBuf::from(line);
            if path.exists() {
                out.push(path);
            }
        }
    }
    for linker in ["/lib", "/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/lib64"] {
        let path = PathBuf::from(linker);
        if path.exists() {
            out.push(path);
        }
    }
    // The interpreter's own directory, which `sys.executable` names as a file.
    if let Some(parent) = python.parent() {
        out.push(parent.to_path_buf());
    }
    out.sort();
    out.dedup();
    out
}
