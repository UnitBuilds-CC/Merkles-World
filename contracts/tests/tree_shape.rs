//! Gate tests for artifact A-10 (`contracts/tree.rs`) — the Merkle tree shape.
//!
//! These are the tests ADR-028's sign-off owed: it locked the digest and named the shape as the
//! thing still unwritten. A peer that disagrees about any of this computes a different epoch root
//! while believing it is consistent, so each rule the shape depends on is pinned here as a literal
//! rather than left to prose. The digest values themselves are pinned by
//! `crates/ledger/tests/roots.rs`, which prints them across architectures for gate P1.5.

use contracts::delta::{INTRA_REGION_BITS, LOCAL_AXIS_BITS};
use contracts::tree::{
    octant_at, preimage, DOMAIN, LEVEL_LEAF, LIST_ARITY, NODE_EMPTY_LIST, NODE_EMPTY_OCTREE,
    NODE_ENTITY_STATE, NODE_EPOCH_ROOT, NODE_EVENT_TAG, NODE_GENESIS, NODE_LIST_BRANCH,
    NODE_OCTANT_BRANCH, NODE_REGION_LEAF, NODE_SLOT_LEAF, NODE_VOXEL_LEAF, OCTREE_ARITY,
    OCTREE_DEPTH,
};

const KINDS: [u8; 11] = [
    NODE_VOXEL_LEAF,
    NODE_OCTANT_BRANCH,
    NODE_EMPTY_OCTREE,
    NODE_EMPTY_LIST,
    NODE_EVENT_TAG,
    NODE_ENTITY_STATE,
    NODE_REGION_LEAF,
    NODE_SLOT_LEAF,
    NODE_LIST_BRANCH,
    NODE_EPOCH_ROOT,
    NODE_GENESIS,
];

/// The kind byte is the only thing separating an octree node from an epoch root in the preimage, so
/// two kinds sharing a byte would let one node type impersonate another. Cheapest possible proof
/// that it does not happen: sort a copy and require every neighbour to differ.
#[test]
fn node_kinds_are_unique() {
    let mut sorted = KINDS;
    sorted.sort_unstable();
    for pair in sorted.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "two node kinds share the byte {}",
            pair[0]
        );
    }
    assert_eq!(sorted.len(), KINDS.len());
}

/// An empty region and an empty epoch must not collide. Both are "nothing here", and that is
/// exactly the pair a shared digest would confuse.
#[test]
fn the_two_empty_rules_are_different_kinds() {
    assert_ne!(NODE_EMPTY_OCTREE, NODE_EMPTY_LIST);
    assert_ne!(
        preimage(NODE_EMPTY_OCTREE, LEVEL_LEAF, &[], &mut [0u8; 64]),
        preimage(NODE_EMPTY_LIST, LEVEL_LEAF, &[], &mut [0u8; 64]),
        "the grammar's separation lives in the kind byte; if these match, no digest can differ"
    );
}

/// ADR-029's 36-bit intra-region address is *why* the octree is 12 levels deep with 8-way
/// branching: one level consumes one bit of each axis. If the region edge ever stops being 4096,
/// this is the test that says so.
#[test]
fn depth_and_arity_come_from_the_region_geometry() {
    assert_eq!(OCTREE_DEPTH, LOCAL_AXIS_BITS);
    assert_eq!(
        OCTREE_DEPTH * 3,
        INTRA_REGION_BITS,
        "each level must consume exactly one bit of x, y and z"
    );
    assert_eq!(OCTREE_ARITY, 1 << 3);
    assert_eq!(LIST_ARITY, 2);
}

/// Child order is the shape decision a peer is most likely to get subtly wrong, so the eight
/// octants are pinned as literals rather than described. x is the low bit, y the middle, z the
/// high one, at every level.
#[test]
fn octant_order_is_x_lowest() {
    // Level 12 is the region root: it selects on the top bit, splitting the 4096³ region in half
    // along every axis.
    assert_eq!(octant_at(0, 0, 0, 12), 0);
    assert_eq!(octant_at(2048, 0, 0, 12), 1);
    assert_eq!(octant_at(0, 2048, 0, 12), 2);
    assert_eq!(octant_at(2048, 2048, 0, 12), 3);
    assert_eq!(octant_at(0, 0, 2048, 12), 4);
    assert_eq!(octant_at(2048, 0, 2048, 12), 5);
    assert_eq!(octant_at(0, 2048, 2048, 12), 6);
    assert_eq!(octant_at(2048, 2048, 2048, 12), 7);

    // Level 1 is the branch whose children are single voxels: bit 0 of each axis, same x-low order.
    assert_eq!(octant_at(1, 0, 0, 1), 1);
    assert_eq!(octant_at(0, 1, 0, 1), 2);
    assert_eq!(octant_at(0, 0, 1, 1), 4);
    assert_eq!(octant_at(1, 1, 1, 1), 7);
    // Parity of the two ends: only the bit read changes between levels, never the axis order.
    assert_eq!(octant_at(2048, 0, 0, 12), octant_at(1, 0, 0, 1));

    // A level outside 1..=12 has no octant and must not wrap a shift into a plausible answer.
    assert_eq!(octant_at(2048, 2048, 2048, 0), 0);
    assert_eq!(octant_at(2048, 2048, 2048, OCTREE_DEPTH + 1), 0);
}

/// The completeness property the whole addressing scheme rests on: walking the octant sequence from
/// the root to a leaf and re-reading one bit per axis per level reconstructs the voxel exactly. No
/// flat voxel index is involved, which is what docs/02 § Octree keys and Morton codes forbids.
///
/// Cases are in `LocalOffset`'s domain (A-01: `0..=4095` per axis) and cover both ends of the
/// 12-bit span, since those are the bits a wrong level convention moves.
#[test]
fn an_octant_path_reconstructs_the_voxel() {
    let cases = [
        (0u16, 0u16, 0u16),
        (4095, 4095, 4095),
        (2048, 0, 0),
        (0, 4095, 17),
        (1234, 567, 3021),
        (4095, 1, 2048),
        (2049, 1023, 4094),
    ];
    for (x, y, z) in cases {
        let (mut rx, mut ry, mut rz) = (0u16, 0u16, 0u16);
        for level in 1..=OCTREE_DEPTH {
            let octant = octant_at(x, y, z, level) as u16;
            // The same bit the level read: `octant_at` consumes bit `level - 1`, because `level`
            // counts branch height up from the voxel leaves rather than down from the region top.
            let bit = 1u16 << (level - 1);
            if octant & 1 != 0 {
                rx |= bit;
            }
            if octant & 2 != 0 {
                ry |= bit;
            }
            if octant & 4 != 0 {
                rz |= bit;
            }
        }
        assert_eq!((rx, ry, rz), (x, y, z), "the path lost a bit somewhere");
    }

    // Out of domain, the answer is truncation and not a wrapped index: only the low 12 bits are
    // read, which is exactly what A-03's masked `DeltaRecord::new` stores. A binding that interleaved
    // 16-bit axes instead would place the same input in a different octant.
    assert_eq!(octant_at(4096, 0, 0, 12), 0);
    assert_eq!(octant_at(8901, 0, 0, 1), octant_at(8901 & 4095, 0, 0, 1));
}

/// Every preimage is `DOMAIN ‖ kind ‖ level ‖ payload`, exactly. A binding that reorders these
/// bytes produces a different root with no error raised, so the layout is asserted as bytes.
#[test]
fn preimage_is_domain_then_kind_then_level_then_payload() {
    let mut buf = [0u8; 64];
    let got = preimage(NODE_VOXEL_LEAF, LEVEL_LEAF, &[0x34, 0x12], &mut buf).unwrap();
    let mut want: Vec<u8> = DOMAIN.to_vec();
    want.push(NODE_VOXEL_LEAF);
    want.push(LEVEL_LEAF);
    want.extend_from_slice(&[0x34, 0x12]);
    assert_eq!(got, want.as_slice());
    assert_eq!(got.len(), DOMAIN.len() + 2 + 2);

    // A buffer with room to spare still reports exactly the bytes it wrote, and a short one
    // refuses rather than hashing a truncated preimage.
    let mut big = [0u8; 4096];
    assert_eq!(preimage(NODE_GENESIS, 0, &[7], &mut big).unwrap().len(), 11);
    assert_eq!(preimage(NODE_GENESIS, 0, &[7], &mut [0u8; 10]), None);
}

/// Maintainability guard, not a design gate: docs/03 § The tree shape mirrors this file, and a spec
/// that quietly diverges from the contract is the defect AGENTS.md §8 calls a broken build. Comments
/// may drift; code may not.
#[test]
fn tree_contract_matches_spec_document() {
    let doc = include_str!("../../docs/03-state-ledger.md");
    let contract = include_str!("../../contracts/tree.rs");
    let block = doc
        .split("```rust")
        .find(|b| b.contains("pub const NODE_VOXEL_LEAF"))
        .and_then(|b| b.split("```").next())
        .unwrap_or_else(|| panic!("docs/03 must keep a ```rust block holding the A-10 contract"));
    assert_eq!(
        code_only(block),
        code_only(contract),
        "contracts/tree.rs and docs/03 § The tree shape have diverged"
    );
}

/// The epoch root's field order is the single line in docs/03 that a reader copies by hand into an
/// implementation, and reordering it produces a valid-looking digest that no other peer computes.
/// Prose numbers drift; this asserts the prose still agrees with `NODE_EPOCH_ROOT`'s payload.
#[test]
fn docs03_states_the_epoch_root_field_order() {
    let doc = include_str!("../../docs/03-state-ledger.md");
    let at = doc
        .find("epoch_root = H(kind = NODE_EPOCH_ROOT")
        .expect("docs/03 must state the epoch root's preimage");
    let line = &doc[at..at + 400];
    let order = [
        "genesis_root",
        "slot_table_root",
        "region_list_root",
        "event_list_root",
    ];
    let mut last = 0usize;
    for field in order {
        let here = line
            .find(field)
            .unwrap_or_else(|| panic!("docs/03's epoch root omits {field}"));
        assert!(
            here > last,
            "docs/03 lists {field} out of order; the preimage is genesis, slot table, regions, events"
        );
        last = here;
    }
}

fn code_only(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .map(str::to_owned)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect()
}
