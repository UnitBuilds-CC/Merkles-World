//! Reading assets back - with the bar set at "parse what `flatc` wrote", not "parse what we wrote".
//!
//! `fixtures/probe_shape.flatc.bin` is packed by flatc 24.3.25 from
//! `contracts/tests/fixtures/probe_shape.json`, the same JSON the `asset schema (flatc)` CI job
//! packs. It is committed because a test cannot run a compiler it does not have, and binary on
//! purpose: the point is that this reader walks the reference implementation's bytes, so a
//! writer and reader that agree with each other and disagree with FlatBuffers fail here instead of
//! shipping.
//!
//! Also checked: refusal. A loader that guesses at a malformed buffer shows the player a wrong world;
//! a loader that errors shows them nothing.

use assetc::reader::{from_bytes, ReadError, FILE_IDENTIFIER};
use assetc::writer::to_bytes;
use assetc::{lower, Shape};
use contracts::sdf::{FLAG_IS_ROOT, GRAMMAR_VERSION, NODE_ROUNDED_BOX};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn flatc_packed_bytes_are_readable_by_our_loader() {
    let bytes =
        std::fs::read(fixture("probe_shape.flatc.bin")).expect("the flatc fixture is committed");
    let asset = from_bytes(&bytes).expect("a buffer written by flatc must parse");
    assert_eq!(asset.depth, 1);
    assert_eq!(asset.nodes.len(), 1);
    // grammar_version was omitted by the writer because it equals the schema default; the reader has
    // to apply the default rather than read whatever bytes happen to follow.
    assert_eq!(asset.grammar_version, 1);
    assert_eq!(asset.params, vec![1.0, 0.0, 0.0, 0.0]);
    assert!(
        asset.label.starts_with("probe:"),
        "label was {}",
        asset.label
    );
    // The one node is what the fixture's JSON says it is: A-07 kind 1, a root with no children and
    // four parameters. The label text calls it a sphere, and the reader does not care - a label is
    // prose for a human, the record is the shape. CI greps this same `kind_and_arity: 1` out of
    // flatc's own read-back, so the committed bytes, the CI job and this test hold one number.
    assert_eq!(asset.nodes[0].kind(), NODE_ROUNDED_BOX);
    assert_eq!(asset.nodes[0].arity(), 0);
    assert_eq!(asset.nodes[0].subtree_len(), 1);
    assert_eq!(asset.nodes[0].params_first(), 0);
    assert_eq!(asset.nodes[0].flags(), FLAG_IS_ROOT);
    assert_eq!(asset.nodes[0].params_len(), 4);
    assert!(asset.nodes[0].arity_matches_the_table());
}

#[test]
fn what_we_write_and_what_we_read_describe_one_shape() {
    let authored = Shape::Union {
        a: Box::new(Shape::Sphere {
            r: 0.7,
            centre: [-0.9, 0.0, 0.0],
        }),
        b: Box::new(Shape::RoundedBox {
            half: [0.35, 0.35, 0.9],
            r: 0.12,
        }),
    };
    let lowered = lower(&authored).expect("lowers");
    let bytes = to_bytes(&lowered, "round trip");
    let read = from_bytes(&bytes).expect("our own output must load");
    assert_eq!(read.nodes.len(), lowered.tree.nodes.len());
    assert_eq!(read.params, lowered.tree.params);
    assert_eq!(read.depth, lowered.depth);
    assert_eq!(read.label, "round trip");
    for (written, back) in lowered.tree.nodes.iter().zip(read.nodes.iter()) {
        assert_eq!(written.0, back.0, "a node must survive the trip unchanged");
    }
}

#[test]
fn malformed_buffers_are_refused_not_guessed() {
    let good = to_bytes(
        &lower(&Shape::Sphere {
            r: 1.0,
            centre: [0.0; 3],
        })
        .expect("lowers"),
        "refusals",
    );
    assert_eq!(from_bytes(&good[0..8]), Err(ReadError::Truncated));

    let mut wrong_id = good.clone();
    wrong_id[4..8].copy_from_slice(b"XXXX");
    assert_eq!(from_bytes(&wrong_id), Err(ReadError::WrongIdentifier));

    // A root offset beyond the buffer is the classic truncated-file case.
    let mut bad_root = good.clone();
    bad_root[0..4].copy_from_slice(&(good.len() as u32 + 64).to_le_bytes());
    assert_eq!(from_bytes(&bad_root), Err(ReadError::Corrupt));

    // A vtable whose declared size runs past the start of the table is a buffer whose parts do not
    // fit together, and reading it would walk into the node data.
    let mut wide = good.clone();
    let table = u32::from_le_bytes(wide[0..4].try_into().unwrap()) as usize;
    let soffset = u32::from_le_bytes(wide[table..table + 4].try_into().unwrap()) as usize;
    let vtable = table - soffset;
    wide[vtable..vtable + 2].copy_from_slice(&60u16.to_le_bytes());
    assert_eq!(from_bytes(&wide), Err(ReadError::Corrupt));
    assert_eq!(FILE_IDENTIFIER, *b"MLWS");
}

/// A vtable with more slots than this schema has fields belongs to some other table. Refusing it is
/// the difference between "this is not a shape asset" and "here is a shape asset with five fields
/// guessed from six".
///
/// Hand-built rather than a mutated `to_bytes` output: our writer puts its vtable immediately before
/// its table, so a vtable wide enough to reach six slots always trips the bounds check first.
#[test]
fn a_vtable_from_a_different_schema_is_refused() {
    let buf = hand_built_table(16, 20, &[0, 0, 0, 0, 0, 0]);
    assert_eq!(from_bytes(&buf), Err(ReadError::UnsupportedField));

    // And with the vtable's own size brought back to this schema's five slots, the same buffer reads
    // as an all-defaults asset instead of being refused.
    let asset =
        from_bytes(&hand_built_table(14, 20, &[0, 0, 0, 0, 0])).expect("five slots is this schema");
    assert_eq!(
        asset.grammar_version, 1,
        "absent field, so the schema default"
    );
    assert!(asset.nodes.is_empty());
}

/// doc 09's other half of the version story: the field exists so a mismatched asset costs one
/// comparison rather than a silently misinterpreted kind table. A grammar-2 container would mean kind
/// numbers somewhere else, so reading its nodes at all is the bug - which is why the refusal happens
/// before the tree is walked.
#[test]
fn a_container_from_another_grammar_is_refused_before_anything_is_parsed() {
    let mut buf = hand_built_table(14, 20, &[4, 0, 0, 0, 0]);
    buf[28..32].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        from_bytes(&buf),
        Err(ReadError::GrammarVersionMismatch {
            found: 2,
            expected: GRAMMAR_VERSION,
        })
    );
    // The same buffer declaring the version this reader implements is an ordinary, if empty, asset.
    buf[28..32].copy_from_slice(&GRAMMAR_VERSION.to_le_bytes());
    assert!(from_bytes(&buf).is_ok());
}

/// A buffer with the layout `writer::to_bytes` produces - root offset, identifier, vtable below the
/// table - so a test can state one header field exactly. `to_bytes` cannot express "declare grammar
/// version 2" or "advertise six slots", because it only writes what the contract says, and the
/// refusals above are about buffers that do not.
///
/// Table at 24, vtable at 8, so the soffset is always 16; `slots` are the vtable's per-field offsets.
fn hand_built_table(vt_size: u16, obj_size: u16, slots: &[u16]) -> Vec<u8> {
    let mut buf = vec![0u8; 64];
    buf[0..4].copy_from_slice(&24u32.to_le_bytes());
    buf[4..8].copy_from_slice(b"MLWS");
    buf[8..10].copy_from_slice(&vt_size.to_le_bytes());
    buf[10..12].copy_from_slice(&obj_size.to_le_bytes());
    for (index, value) in slots.iter().enumerate() {
        buf[12 + index * 2..14 + index * 2].copy_from_slice(&value.to_le_bytes());
    }
    buf[24..28].copy_from_slice(&16u32.to_le_bytes());
    buf
}
