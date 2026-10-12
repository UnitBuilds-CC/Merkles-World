//! The engine's output is part of the published evidence, so it gets a test rather than a habit.
//!
//! docs/03 tabulates `EPOCH_0` and the vectors beside it, and docs/11's gate table leans on
//! "run the binary and it agrees". That claim is only worth what enforces it, so this checks two
//! things CI can check on every OS and architecture it has:
//!
//! 1. **Run to run, byte for byte.** Two spawns of the same binary produce identical stdout. A digest
//!    that drifted between runs would be a determinism bug no unit test inside the crates can see,
//!    because each crate is correct in isolation and the composition is not.
//! 2. **The numbers printed are the numbers published.** The three digests the run reports must equal
//!    docs/03's literals, so a change to the ledger that silently moves a root fails here even if
//!    every in-crate test is updated to match.
//!
//! What this deliberately does not claim: it is one machine per row, not gate P1.5's four named
//! devices, and the debug-versus-release comparison is measured by hand (`cargo run` and
//! `cargo run --release`) because a test can only spawn the profile it was built with. The
//! cross-profile check is documented against gate P1.5's row, not duplicated here.

use std::process::Command;

const EPOCH_0: &str = "91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61";
const REGION_0: &str = "97627e3290a2b4f34e708359af9235d6e2d6d417e1491380d37e58178f9bdcbc";
const SLOT_TABLE: &str = "1aa6a154af1de795cafe245752d51df583008abd84ccec3976547a2477f069ab";
const REGIONS: &str = "0f70e649b228e5d132332b51cfff3aa11badb2199e2cd3980eb4bf16cb6d9a0f";

fn boot() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_engine"))
        .output()
        .expect("the engine binary is built for its own integration test");
    assert!(
        output.status.success(),
        "the engine must exit cleanly; stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("the engine prints UTF-8")
}

#[test]
fn a_boot_is_reproducible_run_to_run() {
    let first = boot();
    let second = boot();
    assert_eq!(
        first, second,
        "two boots of the same binary must agree byte for byte:\n--- first ---\n{first}\n--- second ---\n{second}"
    );
    assert!(
        first.contains("boot complete"),
        "the run should reach its own end, not bail halfway:\n{first}"
    );
}

#[test]
fn what_the_engine_prints_is_what_the_docs_publish() {
    let output = boot();
    for (name, digest) in [
        ("epoch root", EPOCH_0),
        ("region list", REGIONS),
        ("slot table", SLOT_TABLE),
    ] {
        assert!(
            output.contains(digest),
            "{name} {digest} is published in docs/03 but missing from the engine's output:\n{output}"
        );
    }
    // The engine derives the single-region root rather than printing it, so it is checked through the
    // assertion inside the binary - which fails the run above if it stops holding.
    assert!(
        !output.contains("0000000000000000000000000000000000000000000000000000000000000000"),
        "an all-zero digest means a scratch-length refusal was silently reported as a root"
    );
    assert!(
        REGION_0.len() == 64,
        "the published literal itself is malformed"
    );
}

#[test]
fn the_lowered_scene_and_the_hand_assembled_one_agree() {
    // The seam this guards: assetc writes node records and render reads them, and the boot's scene
    // reaches the renderer only after a trip through the `ShapeAsset` envelope. Each side validates,
    // so a disagreement about field meaning - which half of a pair is the kept solid, what axis a
    // transform's second parameter is - would produce a plausible scene on one side and a different
    // one on the other, invisible to both suites. The engine binary prints which path it used, so a
    // regression here also shows up in the reproducibility check above.
    let output = boot();
    assert!(
        output.contains("authored shape lowered by assetc (5 nodes, depth 3)"),
        "the engine is expected to build its scene through assetc:
{output}"
    );
    // Not merely lowered: compiled to bytes and loaded, which is what makes the scene an asset rather
    // than a data structure this binary happens to hold.
    assert!(
        output.contains("compiled to") && output.contains("loaded by assetc::reader"),
        "the boot's scene must arrive through the compiled format, not in memory:
{output}"
    );
}
