//! The published A-10 root vectors, and the structural rules they depend on.
//!
//! ADR-028 promised "published test vectors so Rust, WASM and any future language binding agree
//! bit-for-bit"; until this file nothing in the repo had ever computed a digest. Each literal is what
//! `cargo run --release --example root_vectors -p ledger` prints, and CI runs that example on x86-64
//! Linux, x86-64 Windows and ARM64 macOS — so the literals are the claim and the jobs are the check.
//!
//! The literals alone would not catch a rule change that happens to leave this scenario intact, so
//! each rule A-10 states gets its own test: the derived empty ladder, the child order, the list's
//! single-item and odd-trailing cases, the payload mask, and the slot-table hazard ADR-029 exists to
//! close.

use contracts::coords::{LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, EpochHeader, RegionSlot};
use contracts::tree::{
    preimage, LEVEL_LEAF, NODE_LIST_BRANCH, NODE_OCTANT_BRANCH, NODE_REGION_LEAF, NODE_SLOT_LEAF,
};
use ledger::{
    epoch_root, event_leaf, genesis_root, list_root, node, node_parts, octant_branch, region_leaf,
    region_root, slot_leaf, slot_leaves, voxel_leaf, EmptyLadder, Leaf, Root,
};

const EMPTY_0: &str = "427c29d266d450a69922e414274d5a1629d0d64eb3a14287b64e39ee15fe5c76";
const EMPTY_12: &str = "15773ea10c73687eba1425aa12f0979c80b072f699d1e4fecebf85a23553d4ce";
const LEAF_0ABC: &str = "48cb92891609b2b4c1666825fc734a3beec5f892dba4d4643530dbeba9aed21c";
const LEAF_0FFF: &str = "f396d68bd4df28e86892b9347bfa839639aa9d9c0a0c0ec938fc48346d33d4cc";
const GENESIS: &str = "a367342aaf99f03043f1bf26431e370a66018052e405a3d5d60f9b6436456fee";
const SLOT_LEAF_0: &str = "e33aa6d522d2842324a0e67100616ebe80d47694685f2dce62dc370e591b598c";
const SLOT_LEAF_1: &str = "305ed78e618947ab1bfab3e93048a0470f5e5412526af121d8cfacb4b7fa55ca";
const SLOT_TABLE: &str = "1aa6a154af1de795cafe245752d51df583008abd84ccec3976547a2477f069ab";
const REGION_0: &str = "97627e3290a2b4f34e708359af9235d6e2d6d417e1491380d37e58178f9bdcbc";
const REGIONS: &str = "0f70e649b228e5d132332b51cfff3aa11badb2199e2cd3980eb4bf16cb6d9a0f";
const EVENTS_ONE: &str = "ef8d20bb6bf31520bc8188ef8f9b47926536e3edd446999070c091d822fb2795";
const EVENTS_EMPTY: &str = "23b60201bc628839bd07b1d9f891798e30f79183a4db0b1bcbcaefd235238cb8";
const EVENTS_TWO: &str = "87b6b9984adcd5486c24746571ec399b04cb7459d995fec050ebfa9f5239bb10";
const EPOCH_0: &str = "91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61";
const EPOCH_1: &str = "44de758bdfb3eff0c7b431f78ae9b9a8bae4f5c02972e5c60da79c11ccb379a7";

const SEED: u64 = 0x5EED_0000_0000_0001;
const SLOTS: [RegionKey; 2] = [
    RegionKey { x: 0, y: 0, z: 0 },
    RegionKey { x: -1, y: 2, z: 0 },
];

fn hex(root: &Root) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The three edits of the published scenario, in **arrival** order — the order the epoch received
/// them, which is deliberately not the order the octree visits them.
fn arrival_leaves() -> [Leaf; 3] {
    [
        Leaf::from_record(DeltaRecord::new(
            RegionSlot(0),
            LocalOffset {
                x: 2048,
                y: 0,
                z: 4095,
            },
            0x7,
        )),
        Leaf::from_record(DeltaRecord::new(
            RegionSlot(0),
            LocalOffset { x: 0, y: 0, z: 0 },
            0x1,
        )),
        Leaf::from_record(DeltaRecord::new(
            RegionSlot(0),
            LocalOffset {
                x: 4095,
                y: 4095,
                z: 4095,
            },
            0x0ABC,
        )),
    ]
}

fn event_one() -> Root {
    event_leaf(1, &[0x01, 0x02])
}

#[test]
fn published_vectors_are_frozen() {
    let ladder = EmptyLadder::compute();
    let genesis = genesis_root(SEED, 0, 0);

    let mut leaves = arrival_leaves();
    let region = region_root(&mut leaves, &ladder);

    let mut slot_buffer = [[0u8; 32]; 2];
    let rows = slot_leaves(&SLOTS, &mut slot_buffer).expect("two slots, two rows");
    let slot_table = list_root(rows);

    let regions = list_root(&[region_leaf(0, &region), region_leaf(1, &ladder.empty(12))]);
    let events = list_root(&[event_one()]);
    let events_two = list_root(&[event_one(), event_leaf(2, &[0xFF])]);

    assert_eq!(hex(&ladder.empty(0)), EMPTY_0);
    assert_eq!(hex(&ladder.empty(12)), EMPTY_12);
    assert_eq!(hex(&voxel_leaf(0x0ABC)), LEAF_0ABC);
    assert_eq!(hex(&genesis), GENESIS);
    assert_eq!(hex(&slot_leaf(0, &SLOTS[0])), SLOT_LEAF_0);
    assert_eq!(hex(&slot_leaf(1, &SLOTS[1])), SLOT_LEAF_1);
    assert_eq!(hex(&slot_table), SLOT_TABLE);
    assert_eq!(hex(&region), REGION_0);
    assert_eq!(hex(&regions), REGIONS);
    assert_eq!(hex(&events), EVENTS_ONE);
    assert_eq!(hex(&list_root(&[])), EVENTS_EMPTY);
    assert_eq!(hex(&events_two), EVENTS_TWO);

    let header = EpochHeader {
        epoch_index: 0,
        base_genesis_root: genesis,
        event_log_root: events,
        region_slots: &SLOTS,
        record_count: 3,
    };
    assert_eq!(
        hex(&epoch_root(&header, &slot_table, &regions, &events)),
        EPOCH_0
    );

    let header_two = EpochHeader {
        epoch_index: 1,
        base_genesis_root: genesis,
        event_log_root: events_two,
        region_slots: &SLOTS,
        record_count: 4,
    };
    assert_eq!(
        hex(&epoch_root(&header_two, &slot_table, &regions, &events_two)),
        EPOCH_1
    );
}

/// docs/03 states the root is **position-keyed**: reordering records that touch different voxels does
/// not change it. Feeding the same three edits in octree-walk order is the direct test — and the
/// reason the doc's old claim that lexicographic sort "is the same order the octree walks" could not
/// survive publication. The walk order is Z-order; neither order moves the root.
#[test]
fn region_root_is_position_keyed_not_arrival_keyed() {
    let ladder = EmptyLadder::compute();

    let mut arrival = arrival_leaves();
    let by_arrival = region_root(&mut arrival, &ladder);

    // The same three positions, already in walk order: octant 0, then 5, then 7 at level 12.
    let mut walked = [
        Leaf {
            x: 0,
            y: 0,
            z: 0,
            payload: 0x1,
        },
        Leaf {
            x: 2048,
            y: 0,
            z: 4095,
            payload: 0x7,
        },
        Leaf {
            x: 4095,
            y: 4095,
            z: 4095,
            payload: 0x0ABC,
        },
    ];
    let by_walk = region_root(&mut walked, &ladder);

    assert_eq!(hex(&by_arrival), REGION_0);
    assert_eq!(by_arrival, by_walk);

    // A genuinely different world really does differ: same positions, one payload changed.
    let mut edited = walked;
    edited[1].payload = 0x8;
    assert_ne!(by_walk, region_root(&mut edited, &ladder));
}

/// The empty ladder derives from one rule, so `empty(12)` must equal a branch hand-built over eight
/// `empty(11)` digests, no two levels may share a digest, and the two empty kinds must stay distinct:
/// an empty region and an empty epoch mean different things.
#[test]
fn empty_ladder_is_derived_and_the_two_empties_differ() {
    let ladder = EmptyLadder::compute();
    let eight = [ladder.empty(11); 8];
    assert_eq!(hex(&ladder.empty(12)), hex(&octant_branch(12, &eight)));
    assert_ne!(ladder.empty(12), list_root(&[]));

    let mut levels = [[0u8; 32]; 13];
    let mut level = 0u32;
    while level < 13 {
        levels[level as usize] = ladder.empty(level);
        level += 1;
    }
    for i in 0..13 {
        for j in (i + 1)..13 {
            assert_ne!(levels[i], levels[j], "levels {i} and {j} collide");
        }
    }
}

/// The child order is x the low bit at every level. For the leaf at `(2048, 0, 0)` that means octant
/// **1** of the level-12 branch — 2 if y were the low bit, 4 if the axes were read z-first. Handing
/// the tree over both ways is the only way to pin "differently fixed orders are different worlds".
#[test]
fn child_order_is_x_low_bit_at_every_level() {
    let ladder = EmptyLadder::compute();
    let mut leaf = [Leaf {
        x: 2048,
        y: 0,
        z: 0,
        payload: 0x1,
    }];
    let built = region_root(&mut leaf, &ladder);

    assert_eq!(hex(&built), hex(&hand_built(&ladder, 0x1, 1)));
    assert_ne!(hex(&built), hex(&hand_built(&ladder, 0x1, 2)));
    assert_ne!(hex(&built), hex(&hand_built(&ladder, 0x1, 4)));
}

/// One edited voxel on a clean region, rebuilt the long way: a leaf, then eleven branches whose only
/// non-empty child is index 0, then the level-12 branch carrying the path at `top_octant`.
fn hand_built(ladder: &EmptyLadder, payload: u16, top_octant: usize) -> Root {
    let mut digest = voxel_leaf(payload);
    let mut level = 1u32;
    while level < 12 {
        let mut children = [ladder.empty(level - 1); 8];
        children[0] = digest;
        digest = octant_branch(level, &children);
        level += 1;
    }
    let mut top = [ladder.empty(11); 8];
    top[top_octant] = digest;
    octant_branch(12, &top)
}

/// A region with no edits is the ladder's top — not a zero, and not an empty list.
#[test]
fn an_untouched_region_is_the_ladders_top() {
    let ladder = EmptyLadder::compute();
    let mut none: [Leaf; 0] = [];
    assert_eq!(region_root(&mut none, &ladder), ladder.empty(12));
    assert_eq!(hex(&ladder.empty(12)), EMPTY_12);
}

/// `list([a]) = a` unwrapped; `list([a, b])` a kind-8 branch; and an odd trailing item paired with
/// **itself**, which is what stops a truncated list agreeing with a longer one at the top.
#[test]
fn list_rules_cover_single_pair_and_odd_trailing() {
    let a = voxel_leaf(0x1);
    let b = voxel_leaf(0x2);
    let c = voxel_leaf(0x3);

    assert_eq!(list_root(&[a]), a, "a one-item list is not wrapped");
    assert_ne!(list_root(&[a]), list_root(&[a, a]));

    let pair = node_parts(NODE_LIST_BRANCH, LEVEL_LEAF, &[&a, &b]);
    assert_eq!(list_root(&[a, b]), pair);
    // A two-item list is *not* a truncated octant branch: kind 8 carries two children, kind 1 eight,
    // and the same two digests fed to the two kinds must not meet.
    let as_octant = octant_branch(0, &[a, b, a, b, a, b, a, b]);
    assert_ne!(pair, as_octant);

    let ab = node_parts(NODE_LIST_BRANCH, LEVEL_LEAF, &[&a, &b]);
    let cc = node_parts(NODE_LIST_BRANCH, LEVEL_LEAF, &[&c, &c]);
    assert_eq!(
        list_root(&[a, b, c]),
        node_parts(NODE_LIST_BRANCH, LEVEL_LEAF, &[&ab, &cc])
    );
    // The rejected alternative — carrying the odd item up unchanged — would make this agree with a
    // two-item list. It must not.
    assert_ne!(
        list_root(&[a, b, c]),
        node_parts(NODE_LIST_BRANCH, LEVEL_LEAF, &[&ab, &c])
    );
}

/// The 12-bit payload field is the only thing a leaf may hash: `0xFFFF` is not a legal payload and
/// rolls into `0x0FFF` rather than widening the leaf (ADR-029's field arithmetic, OQ-45's silence).
#[test]
fn payload_rolls_into_twelve_bits_rather_than_widening() {
    assert_eq!(hex(&voxel_leaf(0xFFFF)), LEAF_0FFF);
    assert_eq!(voxel_leaf(0xFFFF), voxel_leaf(0x0FFF));
    assert_ne!(voxel_leaf(0x0FFF), voxel_leaf(0x0FFE));
}

/// ADR-029's hazard, closed: peers that assigned the same slots to *different* regions must not
/// agree. The mapping is under its own root, so a swap moves it even when every region root matches.
#[test]
fn a_slot_reassignment_moves_the_table_root() {
    let mut forward = [[0u8; 32]; 2];
    let mut swapped = [[0u8; 32]; 2];
    let rows = slot_leaves(&SLOTS, &mut forward).expect("two rows");
    let keys = [SLOTS[1], SLOTS[0]];
    let other = slot_leaves(&keys, &mut swapped).expect("two rows");

    assert_eq!(hex(&list_root(rows)), SLOT_TABLE);
    assert_ne!(list_root(rows), list_root(other));
}

/// A short caller buffer is refused rather than silently truncated — a root over half a table would
/// be worse than no root.
#[test]
fn a_short_slot_buffer_is_refused() {
    let mut one_row = [[0u8; 32]; 1];
    assert!(slot_leaves(&SLOTS, &mut one_row).is_none());
}

/// `contracts/tree.rs`'s reference byte builder and this crate's streaming hasher must produce the
/// same digest, or the contract is decoration. Written out for a slot leaf:
/// `DOMAIN ‖ kind ‖ level ‖ LE_u16(slot) ‖ LE_i64 × 3`.
#[test]
fn streamed_parts_match_the_reference_preimage() {
    let key = RegionKey { x: -1, y: 2, z: 0 };
    let mut payload = [0u8; 26];
    payload[..2].copy_from_slice(&1u16.to_le_bytes());
    payload[2..10].copy_from_slice(&(-1i64).to_le_bytes());
    payload[10..18].copy_from_slice(&2i64.to_le_bytes());
    payload[18..].copy_from_slice(&0i64.to_le_bytes());

    let mut buffer = [0u8; 8 + 2 + 26];
    let bytes = preimage(NODE_SLOT_LEAF, LEVEL_LEAF, &payload, &mut buffer)
        .expect("buffer sized for a slot leaf");
    assert_eq!(&bytes[..8], b"mlworld1");
    assert_eq!(bytes[8], NODE_SLOT_LEAF);
    assert_eq!(bytes[9], LEVEL_LEAF);
    assert_eq!(
        slot_leaf(1, &key),
        *blake3::hash(bytes).as_bytes(),
        "the reference preimage and the hasher disagree"
    );
}

/// A payload-free node still hashes its kind and level, so no two kinds that carry the same payload
/// can collide — the domain-separation half of ADR-028.
#[test]
fn kinds_are_distinguished_by_the_preimage_not_their_payload() {
    assert_ne!(
        node(NODE_LIST_BRANCH, LEVEL_LEAF, b"x"),
        node(NODE_OCTANT_BRANCH, LEVEL_LEAF, b"x")
    );
    assert_ne!(
        node(NODE_SLOT_LEAF, LEVEL_LEAF, &[]),
        node(NODE_REGION_LEAF, LEVEL_LEAF, &[])
    );
    assert_ne!(
        node(NODE_SLOT_LEAF, LEVEL_LEAF, &[]),
        node(NODE_SLOT_LEAF, 1, &[])
    );
}
