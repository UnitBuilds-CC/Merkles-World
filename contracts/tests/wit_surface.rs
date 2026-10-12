//! A-02 exists as a file, so it can be checked rather than read.
//!
//! Why this test is worth a dev-dependency: until A-02 was a real path, `wit-bindgen` had nothing to
//! generate from, and the workspace's `unsafe_code = "forbid"` means a WASM export cannot be
//! hand-written instead - so gate P1.5's browser target and OQ-41 both sat on a prose excerpt. Parsing
//! the file is the smallest claim that is more than "I looked at it". It is also honest about its
//! limits: a parse proves the surface is well-formed WIT and that the names match doc 08, not that
//! fuel metering (OQ-20), the escalation story (OQ-34), or the signing scheme (OQ-46) are settled.
//! Those stay open questions, recorded in the file's own header.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_wit_surface_parses() {
    let path = manifest_dir().join("ledger-api.wit");
    let mut resolve = wit_parser::Resolve::new();
    // `push_path` hands back the package id and its source map; the `Ok` is the parse. The map is
    // deliberately unchecked here - asserting on a type whose accessors this test would then be
    // coupled to buys nothing beyond what `push_path` already refused.
    let (package, _sources) = resolve
        .push_path(&path)
        .unwrap_or_else(|error| panic!("{} must parse as WIT: {error}", path.display()));

    let name = resolve.packages[package].name.to_string();
    assert_eq!(
        name, "game:modding@0.1.0",
        "the package and version in A-02 are what doc 08's excerpt declares; changing them is a \
         contract change (migration note + version bump + ADR), not an edit",
    );

    let mut interfaces: Vec<&str> = resolve
        .interfaces
        .iter()
        .filter_map(|(_, interface)| interface.name.as_deref())
        .collect();
    interfaces.sort_unstable();
    assert_eq!(
        interfaces,
        vec!["ledger-api", "query-api"],
        "doc 08 names exactly these two imported interfaces",
    );

    let mut worlds: Vec<&str> = resolve
        .worlds
        .iter()
        .map(|(_, world)| world.name.as_str())
        .collect();
    worlds.sort_unstable();
    assert_eq!(worlds, vec!["mod-plugin"], "one world per plugin surface");
}

/// The mirror discipline the rest of `contracts/` follows, applied to the one thing here that is not
/// Rust: the file on disk and the excerpt in doc 08 must not drift into two surfaces. Comparing the
/// declarations rather than the prose keeps the check meaningful without making the doc's commentary
/// part of the contract.
#[test]
fn the_surface_matches_the_doc_excerpt() {
    let contract =
        std::fs::read_to_string(manifest_dir().join("ledger-api.wit")).expect("A-02 exists");
    let doc = std::fs::read_to_string(manifest_dir().join("../docs/08-wasm-modding.md"))
        .expect("doc 08 exists");

    let block = doc
        .split("```wit")
        .find(|candidate| candidate.contains("world mod-plugin"))
        .and_then(|candidate| candidate.split("```").next())
        .unwrap_or_else(|| panic!("doc 08 must keep a ```wit block holding the surface"));

    for declaration in [
        "package game:modding@0.1.0;",
        "interface ledger-api {",
        "interface query-api {",
        "world mod-plugin {",
        "get-region-macro-state: func(region: region-key) -> option<macro-state>;",
        "get-field: func(region: region-key, field: field-id) -> option<s64>;",
        "get-delta: func(pos: global-voxel-pos) -> option<u64>;",
        "emit-event-tag: func(event: event-tag);",
        "find-regions: func(predicate: list<u8>) -> list<region-key>;",
        "count-bytes-read: func() -> u64;",
        "export on-load: func(manifest: list<u8>);",
        "export on-epoch-start: func(current-epoch: u64);",
        "export evaluate-circuit-node: func(inputs: list<bool>) -> list<bool>;",
        "export step-macro-field: func(region: region-key, field: field-id, dt: u64,
                                 current: s64) -> s64;",
    ] {
        assert!(
            block.contains(declaration),
            "doc 08 no longer states `{declaration}`; the doc is the spec, so reconcile it with A-02 \
             in the same change rather than letting either one drift",
        );
        assert!(
            contract.contains(declaration),
            "A-02 dropped `{declaration}`, which doc 08 still declares",
        );
    }
}
