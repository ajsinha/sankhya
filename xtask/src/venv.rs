//! The Python environment this repository pins, and the one command that builds it.
//!
//! # Why a version pin, for a language nothing here is written in
//!
//! Six things run in Python: the mutation audit, the SDK examples, the parity soak, the TLS
//! binding test, the deck's renderer and its geometry audit. Every one of them used to take
//! whatever `python3` the machine's `PATH` offered.
//!
//! `rust-toolchain.toml` pins the compiler and argues, in as many words, that a version which
//! behaves differently on one machine turns the gate into a property of whoever ran it. That
//! argument does not stop at the language boundary --- least of all for `python-pptx`, which
//! lays out a slide and is not installed system-wide anywhere.

use std::path::Path;
use std::process::Command;

/// Build `.venv` from the pinned interpreter, and install what `requirements.txt` names.
///
/// # Why this is a task and not three lines in a README
///
/// It was three lines in a README, and they said `python3 -m venv .venv` --- so the environment
/// the deck was rendered in was whatever the machine's `PATH` happened to offer. This
/// repository pins its compiler in `rust-toolchain.toml` and argues, correctly, that a version
/// which behaves differently on one machine turns a green run into a property of whoever ran
/// it. The same argument applies to the interpreter that lays out a slide.
///
/// The version comes from `.python-version` and the packages from `requirements.txt`, so this
/// is the one place either is named.
///
/// # Errors
///
/// Reports and returns false when the pinned interpreter cannot be found, rather than falling
/// back to another one. A fallback here would rebuild the environment this exists to pin.
pub(crate) fn make(root: &Path) -> bool {
    println!("== venv");
    let Ok(pinned) = std::fs::read_to_string(root.join(".python-version")) else {
        eprintln!("  MISSING  .python-version does not exist, so there is no version to build");
        return false;
    };
    let pinned = pinned.trim().to_string();

    let Some(interpreter) = interpreter_for(&pinned) else {
        eprintln!(
            "  NOT FOUND  no interpreter reporting {pinned} was found. Looked in \
             ~/.local/share/uv/python/, then at `python3` on the path. Install it --- \
             `uv python install {pinned}` --- rather than relaxing the pin: an environment \
             built on a different version is not the environment this repository describes"
        );
        return false;
    };
    println!("   {pinned} at {}", interpreter.display());

    let venv = root.join(".venv");
    if venv.exists() && std::fs::remove_dir_all(&venv).is_err() {
        eprintln!("  IN USE   .venv could not be replaced");
        return false;
    }
    let built = Command::new(&interpreter)
        .current_dir(root)
        .args(["-m", "venv", ".venv"])
        .status();
    if !built.is_ok_and(|status| status.success()) {
        eprintln!("  FAILED   the interpreter could not create .venv");
        return false;
    }

    let installed = Command::new(venv.join("bin").join("pip"))
        .current_dir(root)
        .args(["install", "--quiet", "--requirement", "requirements.txt"])
        .status();
    if !installed.is_ok_and(|status| status.success()) {
        eprintln!("  FAILED   requirements.txt did not install");
        return false;
    }

    println!("   .venv holds what requirements.txt names");
    true
}

/// An interpreter reporting exactly this version, or none.
///
/// uv keeps its interpreters in a predictable place and names the directory after the version,
/// so that is looked at first --- and the answer is confirmed by asking the binary rather than
/// trusting the directory name. `python3` on the path is accepted only if it reports the same
/// version, because the point of the pin is that they are not interchangeable.
fn interpreter_for(pinned: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    let uv = std::path::Path::new(&home).join(".local/share/uv/python");
    if let Ok(entries) = std::fs::read_dir(&uv) {
        for entry in entries.flatten() {
            candidates.push(entry.path().join("bin").join("python3"));
        }
    }
    candidates.push(std::path::PathBuf::from("python3"));

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout).trim() == format!("Python {pinned}")
            })
    })
}

/// The Python this repository runs.
///
/// The project's own `.venv` when it is there, `python3` when it is not, and `SANKHYA_PYTHON`
/// over both. Duplicated from `sankhya_testkit::python` rather than shared, deliberately:
/// `xtask` depends on nothing in `crates/` and must keep doing so --- it is the thing that
/// checks the layer graph, and a gate that imports the code it audits is a gate with an
/// opinion. Eleven lines is a smaller cost than that.
pub(crate) fn python(root: &Path) -> std::path::PathBuf {
    if let Ok(named) = std::env::var("SANKHYA_PYTHON") {
        if !named.trim().is_empty() {
            return std::path::PathBuf::from(named);
        }
    }
    let venv = root.join(".venv").join("bin").join("python");
    if venv.exists() { venv } else { std::path::PathBuf::from("python3") }
}
