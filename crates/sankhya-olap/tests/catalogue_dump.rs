//! Dump every function a session registers. Ignored by default; run to regenerate the catalogue.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout)]

#[test]
#[ignore = "a reporting tool, not an assertion"]
fn dump() {
    let context = sankhya_olap::session().expect("a session");
    // What the *server* registers, not what a bare session has --- the vector, matrix and
    // constructor functions are added on top, and a catalogue that listed only the engine's
    // own would be a list of what SANKHYA did not contribute.
    sankhya_olap::register_vector_functions(&context);
    sankhya_olap::register_matrix_functions(&context);
    sankhya_olap::register_constructors(&context);
    let state = context.state();
    let mut names: Vec<String> = Vec::new();
    for name in state.scalar_functions().keys() {
        names.push(format!("scalar\t{name}"));
    }
    for name in state.aggregate_functions().keys() {
        names.push(format!("aggregate\t{name}"));
    }
    for name in state.window_functions().keys() {
        names.push(format!("window\t{name}"));
    }
    names.sort();
    for n in &names {
        println!("{n}");
    }
    println!("TOTAL\t{}", names.len());
}
