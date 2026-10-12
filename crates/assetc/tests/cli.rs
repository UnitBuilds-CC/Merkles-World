//! The compiler command, end to end: a RON file on disk in, a `ShapeAsset` out.
//!
//! Two things are worth pinning here rather than in unit tests, because they only exist at this seam.
//! First, the file the CLI writes has to be a real asset: this test loads it back through
//! `assetc::reader::from_bytes` and hands the result to `crates/render`, so the whole
//! RON-in / distance-field-out path is walked at the seam where it actually exists rather than being
//! asserted piecewise. Second, the CLI has to *fail*: an unparseable authoring file or an
//! uncompilable shape must exit non-zero with a message, because a compiler that prints a plausible
//! file for broken input is worse than one that refuses, and `assetc watch` would otherwise keep a
//! stale asset looking current.
//!
//! The independent verdict on the bytes is CI's, not here: the `asset schema (flatc)` job feeds a
//! written file to the pinned `flatc` parser and asserts what comes back.

use assetc::reader::from_bytes;
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

    // The test's name says *loadable*, so the last word goes to the loader rather than to arithmetic
    // done here: the file the CLI writes must read back and be a tree `crates/render` evaluates. That
    // is the seam the engine will use - RON on disk in, a signed distance field out, nothing parsed at
    // runtime (AGENTS.md §3).
    let loaded =
        from_bytes(&bytes).expect("the CLI's output must load the way the engine loads it");
    assert_eq!(
        loaded.label, "sphere.ron",
        "the label carries the authoring file's name"
    );
    let tree = render::Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("a compiled asset is a valid A-07 tree");
    assert_eq!(tree.nodes.len(), 1);
    // A unit sphere at the origin: one unit inside at the centre, one unit outside at `x = 2`.
    assert_eq!(render::sdf_eval(&tree, 0, [0.0, 0.0, 0.0]), -1.0);
    assert_eq!(render::sdf_eval(&tree, 0, [2.0, 0.0, 0.0]), 1.0);
    let hit = render::march(&tree, 0, [4.0, 0.0, 0.0], [-1.0, 0.0, 0.0], 8.0, 0.001, 64)
        .expect("a ray from x = 4 heading inwards reaches the surface");
    assert!(
        (hit.t - 3.0).abs() < 1e-5,
        "the surface is one unit out, so t = 3; got {}",
        hit.t
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

/// The fixture CI hashes on three architectures, checked here for the two things a digest cannot show:
/// that the bytes are a *loadable* asset rather than arbitrary identical output, and that the
/// reproducibility AS2 claims holds even locally. A cross-machine agreement on a file the reader would
/// reject would be a green checkmark over a broken gate.
#[test]
fn the_cross_machine_fixture_is_loadable_and_repeated() {
    let input = fixture("knot.ron");
    let first = out_path("knot-a.bin");
    let second = out_path("knot-b.bin");

    let a = run(&[input.as_path(), first.as_path()]);
    let b = run(&[input.as_path(), second.as_path()]);
    assert!(a.status.success() && b.status.success());
    let bytes_a = std::fs::read(&first).expect("first compile wrote a file");
    let bytes_b = std::fs::read(&second).expect("second compile wrote a file");
    assert_eq!(
        bytes_a, bytes_b,
        "two compiles of one source differ on one machine, so AS2 cannot be about two"
    );

    // Identical bytes are only useful if they are an asset: read them back through the load path the
    // engine uses and compare against what the library lowers from the same source.
    let loaded = from_bytes(&bytes_a).expect("the fixture's bytes load as a ShapeAsset");
    let text = std::fs::read_to_string(&input).expect("fixture is readable");
    let authored: assetc::Shape = ron::from_str(&text).expect("fixture parses");
    let lowered = assetc::lower(&authored).expect("fixture lowers");
    assert_eq!(loaded.nodes, lowered.tree.nodes);
    assert_eq!(loaded.params, lowered.tree.params);
    assert_eq!(loaded.depth, lowered.depth);
    assert_eq!(
        loaded.label, "knot.ron",
        "the label is part of the bytes two machines are being compared on"
    );

    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
}
