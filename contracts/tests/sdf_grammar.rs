//! Artifact A-07's grammar tests: the kind table is self-consistent, the record layout loses nothing,
//! and a record can be caught lying about its own arity.
//!
//! The second test is the one with history. A-07's first draft packed `params_first`, `params_len`
//! and `flags` into one 32-bit word, and `flags << 24` overwrote the top byte of `params_len` — so a
//! node asking for 0x0abc parameters read back as 0x0acc. Nothing failed to compile and nothing
//! complained at review; the loss was only visible in the reader. That is what a record layout shared
//! between `assetc`, WGSL and a WASM binding must never do, so the case is pinned here rather than
//! trusted to the next pair of eyes.

use contracts::sdf::{
    arity_of, is_known_kind, pack_first_and_flags, pack_kind_and_arity, pack_params_len,
    NodeRecord, FLAG_HAS_MATERIAL, FLAG_IS_ROOT, KNOWN_KINDS, NODE_MATERIAL, NODE_NOISE,
    NODE_SMOOTH_UNION, NODE_SPHERE, NODE_STRIDE_BYTES, NODE_UNION,
};

#[test]
fn kinds_are_unique_and_every_kind_has_an_arity() {
    for (i, a) in KNOWN_KINDS.iter().enumerate() {
        for b in &KNOWN_KINDS[i + 1..] {
            assert_ne!(a, b, "kind {a} is listed twice");
        }
        assert!(is_known_kind(*a), "{a} is listed but not known");
        let arity = arity_of(*a);
        assert_ne!(arity, usize::MAX, "{a} has no arity in the table");
        assert!(
            arity <= 2,
            "{a} fans wider than the binary combiners assetc flattens to"
        );
    }
    // A material leaf carries no shape, and a noise node wraps exactly one child.
    assert_eq!(arity_of(NODE_MATERIAL), 0);
    assert_eq!(arity_of(NODE_NOISE), 1);
    assert_eq!(arity_of(NODE_SPHERE), 0);
    assert_eq!(arity_of(NODE_UNION), 2);
    // An unassigned number is unknown, and its arity is the value no well-formed tree can satisfy.
    assert!(!is_known_kind(63));
    assert_eq!(arity_of(63), usize::MAX);
}

#[test]
fn the_record_round_trips_without_losing_fields() {
    let record = NodeRecord([
        pack_kind_and_arity(NODE_SMOOTH_UNION, 2),
        12,
        pack_first_and_flags(4, FLAG_IS_ROOT | FLAG_HAS_MATERIAL),
        pack_params_len(0x0abc),
    ]);
    assert_eq!(record.kind(), NODE_SMOOTH_UNION);
    assert_eq!(record.arity(), 2);
    assert_eq!(record.subtree_len(), 12);
    assert_eq!(record.params_first(), 4);
    assert_eq!(record.flags(), FLAG_IS_ROOT | FLAG_HAS_MATERIAL);
    assert_eq!(
        record.params_len(),
        0x0abc,
        "the first draft read this back as 0x0acc: flags ate the high byte"
    );

    // Pre-order plus subtree_len: a sibling is arithmetic, not a walk.
    assert_eq!(record.next_sibling(0), 12);
    assert_eq!(record.next_sibling(3), 15);

    // The stride is what the buffer promises, measured rather than declared twice.
    let measured = size_of_val(&record);
    assert_eq!(measured, NODE_STRIDE_BYTES);
}

#[test]
fn a_record_can_be_caught_lying_about_its_arity() {
    let honest = NodeRecord([pack_kind_and_arity(NODE_UNION, 2), 3, 0, pack_params_len(0)]);
    assert!(honest.arity_matches_the_table());

    // The tree an authoring file might smuggle through: a sphere claiming a child, or a union
    // claiming three. assetc refuses both, so the interpreter never has to guess.
    let lying_sphere = NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 1),
        2,
        0,
        pack_params_len(0),
    ]);
    assert!(!lying_sphere.arity_matches_the_table());
    let wide_union = NodeRecord([pack_kind_and_arity(NODE_UNION, 3), 5, 0, pack_params_len(0)]);
    assert!(!wide_union.arity_matches_the_table());
    let unassigned = NodeRecord([pack_kind_and_arity(63, 0), 1, 0, pack_params_len(0)]);
    assert!(
        !unassigned.arity_matches_the_table(),
        "an unknown kind must not validate by accident"
    );
}
