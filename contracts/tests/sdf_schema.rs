//! A-07's other half: the asset envelope (`contracts/sdf.fbs`) against the grammar (`contracts/sdf.rs`).
//!
//! Why a text-comparison test rather than a schema library: the risk this guards is drift between two
//! files that must describe one layout - the record's word order in Rust, and the struct's field order
//! in the schema. A FlatBuffers struct is fixed-layout and inline, so that order *is* the byte layout
//! the WGSL buffer sees, and a silent reordering would reinterpret every authored asset.
//!
//! Verified once with the real compiler, and the claim it confirmed is recorded here rather than
//! assumed: `flatc 24.3.25 --rust` on this schema generates `pub struct NodeRecord(pub [u8; 16])`,
//! aligned to 4 - i.e. the 16-byte promise in `NODE_STRIDE_BYTES` is what a real code generator
//! produces, not just what the doc comment claims. The same compiler packed a probe asset to 156
//! bytes. Neither step is in CI (a pinned flatc download job is the obvious follow-up, modelled on the
//! cargo-deny job), so what CI enforces is the weaker but useful invariant: the two files still say the
//! same thing.

use contracts::sdf::{NodeRecord, GRAMMAR_VERSION, NODE_STRIDE_BYTES};

const SCHEMA: &str = include_str!("../sdf.fbs");

#[test]
fn the_envelopes_node_struct_is_the_records_word_order() {
    let body = SCHEMA
        .split("struct NodeRecord {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("sdf.fbs declares struct NodeRecord");
    let fields: Vec<&str> = body
        .lines()
        .filter_map(|line| line.split(':').next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();

    assert_eq!(
        fields,
        vec![
            "kind_and_arity",
            "subtree_len",
            "first_and_flags",
            "params_len_reserved",
        ],
        "the schema must list the record's words in the order NodeRecord documents, or an asset is \
         read with the wrong field meanings",
    );

    // Four uint32s: the widths are the layout, so this is the 16-byte claim in another form.
    assert!(
        body.matches("uint32").count() == 4,
        "each word is exactly uint32"
    );
    let stride = NODE_STRIDE_BYTES;
    assert_eq!(stride, size_of::<NodeRecord>());
    assert_eq!(stride, 4 * size_of::<u32>());
}

#[test]
fn the_grammar_version_the_envelope_carries_is_the_grammars() {
    let declared = SCHEMA
        .lines()
        .find_map(|line| {
            let rest = line.trim().strip_prefix("grammar_version: uint32 =")?;
            rest.trim().trim_end_matches(';').parse::<u32>().ok()
        })
        .expect("the schema declares a defaulted grammar_version");
    assert_eq!(
        declared, GRAMMAR_VERSION,
        "a bumped GRAMMAR_VERSION with an un-bumped default means assetc writes files the loader \
         accepts and misreads",
    );
}

#[test]
fn the_envelope_names_a_root_and_a_file_identifier() {
    assert!(
        SCHEMA.contains("root_type ShapeAsset;"),
        "no root type means `assetc` has no entry point to pack against"
    );
    assert!(
        SCHEMA.contains("file_identifier"),
        "the loader's one-comparison check against a truncated or misnamed file"
    );
    assert!(
        SCHEMA.contains("nodes: [NodeRecord]"),
        "a table of nodes rather than a struct vector would stop the asset being one buffer"
    );
    assert!(
        SCHEMA.contains("params: [float32]"),
        "the parameter pool the records index"
    );
}
