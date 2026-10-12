//! The compiler command, end to end: a RON file on disk in, a `ShapeAsset` out.
//!
//! Two things are worth pinning here rather than in unit tests, because they only exist at this seam.
//! First, the file the CLI writes has to be a real asset - a reader must be able to follow the root
//! offset and find the identifier, which is the contract the engine's loader will rely on. Second, the
//! CLI has to *fail*: an unparseable authoring file or an uncompilable shape must exit non-zero with a
//! message, because a compiler that prints a plausible file for broken input is worse than one that
//! refuses, and `assetc watch` would otherwise keep a stale asset looking current.
//!
//! The independent verdict on the bytes is CI's, not here: the `asset schema (flatc)` job feeds a
//! written file to the pinned `flatc` parser and asserts what comes back.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn run(args: &[&Path]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_assetc"))
        .args(args)
        .output()
        .expect("assetc is built for its own integration test")
}

fn out_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("assetc-{name}-{}.bin", std::process::id()));
    path
}

#[test]
fn a_ron_file_becomes_a_loadable_asset() {
    let input = fixture("sphere.ron");
    let output = out_path("sphere");
    let result = run(&[input.as_path(), output.as_path()]);
    assert!(
        result.status.success(),
        "the fixture must compile; stderr was {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let bytes = std::fs::read(&output).expect("the output file exists");
    assert!(
        bytes.len() > 24,
        "a one-node asset is small but not trivial: {}",
        bytes.len()
    );
    // The identifier the schema declares, at the documented position, is what lets a loader refuse the
    // wrong file in one comparison.
    assert_eq!(
        &bytes[4..8],
        b"MLWS",
        "the file identifier follows the root uoffset"
    );
    let root = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    assert!(
        root + 4 <= bytes.len(),
        "the root offset must point inside the file"
    );
    let soffset = u32::from_le_bytes(bytes[root..root + 4].try_into().unwrap()) as usize;
    assert!(
        soffset > 0 && soffset <= root,
        "the vtable sits below the table it describes"
    );
    let _ = std::fs::remove_file(&output);
}

#[test]
fn broken_input_is_refused_rather_than_compiled_into_something() {
    let bad = out_path("bad.ron");
    std::fs::write(&bad, "this is not a shape at all").expect("writable");
    let output = out_path("bad.bin");
    let result = run(&[bad.as_path(), output.as_path()]);
    assert!(
        !result.status.success(),
        "a malformed authoring file must fail the build, not emit a file"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("not a valid authoring file"),
        "the refusal should name the reason: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !output.exists(),
        "nothing may be written when compilation fails"
    );
    let _ = std::fs::remove_file(&bad);
}
