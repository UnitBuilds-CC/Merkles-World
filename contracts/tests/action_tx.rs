//! Gate tests for artifact A-08 (`contracts/tx.rs`) — the envelope's own claims, plus the seam
//! where an `ActionTx` becomes the A-03 record a slab stores.

use contracts::coords::LocalOffset;
use contracts::delta::{DeltaRecord, RegionSlot};
use contracts::tx::{
    ActionTx, ActorId, Channel, EntityId, FunctionIndex, Hlc, ModuleId, PeerId, SequenceId,
    Signature, Target, TickEpoch, TxKind, TxPayload, FIELD_ORDER,
};

fn slot(n: u16) -> RegionSlot {
    RegionSlot(n)
}

fn offset() -> LocalOffset {
    LocalOffset {
        x: 12,
        y: 34,
        z: 56,
    }
}

fn seq(hlc: u64, peer: u32) -> SequenceId {
    SequenceId {
        hlc: Hlc(hlc),
        peer: PeerId(peer),
    }
}

const SIGNATURE: &[u8] = &[0u8; 8];

/// One transaction per payload class, all well-formed by construction.
fn tx(kind: TxKind, target: Target, payload: TxPayload<'static>) -> ActionTx<'static> {
    ActionTx {
        sequence: seq(1, 1),
        actor: ActorId(7),
        kind,
        target,
        payload,
        tick_epoch: TickEpoch(3),
        signature: Signature { bytes: SIGNATURE },
    }
}

fn delta_tx() -> ActionTx<'static> {
    tx(
        TxKind::SlabDeltaMutate,
        Target::Voxel {
            slot: slot(9),
            offset: offset(),
        },
        TxPayload::DeltaValue { payload: 0x123 },
    )
}

fn kinematics_tx() -> ActionTx<'static> {
    tx(
        TxKind::EntityKinematics,
        Target::Entity { id: EntityId(4) },
        TxPayload::Kinematics {
            position: [1.0, 2.0, 3.0],
            velocity: [0.0, 0.0, 0.0],
            orientation: [0.0, 0.0, 0.0, 1.0],
        },
    )
}

fn signal_tx() -> ActionTx<'static> {
    tx(
        TxKind::WasmSignalTrigger,
        Target::Module {
            module: ModuleId(2),
            function: FunctionIndex(5),
        },
        TxPayload::Signal { bytes: &[1, 2, 3] },
    )
}

/// The three classes, their tags, and the fact that a tag from a newer build cannot alias one.
#[test]
fn wire_tags_are_literals_not_declaration_order() {
    assert_eq!(TxKind::SlabDeltaMutate.tag(), 1);
    assert_eq!(TxKind::EntityKinematics.tag(), 2);
    assert_eq!(TxKind::WasmSignalTrigger.tag(), 3);
    for kind in [
        TxKind::SlabDeltaMutate,
        TxKind::EntityKinematics,
        TxKind::WasmSignalTrigger,
    ] {
        assert_eq!(TxKind::from_tag(kind.tag()), Some(kind));
    }
    // 0 is the reserved value, and anything past the last tag is unknown rather than a wrap.
    assert_eq!(TxKind::from_tag(0), None);
    assert_eq!(TxKind::from_tag(4), None);
    assert_eq!(TxKind::from_tag(u8::MAX), None);
}

/// ADR-022's two channel classes, derived from the kind rather than chosen per call site: a
/// dropped position frame is free, a dropped break is a desync.
#[test]
fn kinds_pick_the_adr_022_channel() {
    assert_eq!(
        TxKind::EntityKinematics.channel(),
        Channel::Unordered,
        "kinematics are superseded by the next frame"
    );
    assert_eq!(TxKind::SlabDeltaMutate.channel(), Channel::Ordered);
    assert_eq!(TxKind::WasmSignalTrigger.channel(), Channel::Ordered);
    assert_eq!(delta_tx().channel(), Channel::Ordered);
    assert_eq!(kinematics_tx().channel(), Channel::Unordered);
}

/// `Kind` is structurally redundant with `Target` and `TxPayload` — it is the tag a decoder reads
/// *before* parsing them — so the three must agree or the envelope misdescribes itself, and
/// `NetError::ActionTxMalformed` is what a receiver does about that.
#[test]
fn envelope_classes_must_agree() {
    assert!(delta_tx().is_well_formed());
    assert!(kinematics_tx().is_well_formed());
    assert!(signal_tx().is_well_formed());

    let mut crossed = delta_tx();
    crossed.kind = TxKind::EntityKinematics;
    assert!(
        !crossed.is_well_formed(),
        "kind no longer matches its target"
    );

    let mut swapped = tx(
        TxKind::WasmSignalTrigger,
        Target::Entity { id: EntityId(4) },
        TxPayload::Signal { bytes: &[] },
    );
    assert!(!swapped.is_well_formed());
    swapped.target = Target::Module {
        module: ModuleId(2),
        function: FunctionIndex(5),
    };
    assert!(
        swapped.is_well_formed(),
        "an argument-less trigger is still a trigger"
    );

    let mut unsigned = delta_tx();
    unsigned.signature = Signature { bytes: &[] };
    assert!(
        !unsigned.is_well_formed(),
        "the signature is the one field docs/10 refuses to shrink"
    );
}

/// The seam between the two artifacts: ADR-015's wire-delta row says the address half and the value
/// half *are* the record, so an envelope must reassemble into exactly the word the slab stores.
#[test]
fn a_slab_mutation_carries_the_a03_record() {
    let record = delta_tx().delta_record().expect("a delta tx has a record");
    assert_eq!(record.slot(), slot(9));
    assert_eq!(record.intra_region(), offset());
    assert_eq!(record.payload(), 0x123);
    assert_eq!(
        record,
        DeltaRecord::new(slot(9), offset(), 0x123),
        "wire fields must tile the record with nothing added"
    );
    assert_eq!(kinematics_tx().delta_record(), None);
    assert_eq!(signal_tx().delta_record(), None);
}

/// Causal order is Lamport, not wall-clock (docs/10 rule 2), and the tie-break is what makes it
/// total: peers at the same reading still apply the pool in one order, which is gate M2's premise.
#[test]
fn sequence_ids_are_totally_ordered() {
    let ids = [
        seq(0, 0),
        seq(0, 1),
        seq(1, 0),
        seq(1, 7),
        seq(9, 3),
        seq(u64::MAX, 0),
    ];
    for &a in &ids {
        for &b in &ids {
            let (before, after, equal) = (a.precedes(&b), b.precedes(&a), a == b);
            assert!(!(before && after), "{a:?} and {b:?} order both ways");
            assert!(
                before || after || equal,
                "{a:?} and {b:?} are unordered — a tie-break is missing"
            );
        }
    }
    assert!(seq(1, 9).precedes(&seq(2, 0)), "hlc dominates the peer id");
    assert!(seq(1, 0).precedes(&seq(1, 1)), "the peer id breaks the tie");
    assert!(
        !seq(1, 1).precedes(&seq(1, 1)),
        "equal ids are equal, not concurrent"
    );
}

/// Maintainability guard: docs/10 § The ActionTx contract mirrors this file. Same rule as coords
/// and delta — comments may drift, code may not.
#[test]
fn tx_contract_matches_spec_document() {
    let doc = include_str!("../../docs/10-multiplayer.md");
    let contract = include_str!("../../contracts/tx.rs");
    let block = doc
        .split("```rust")
        .find(|b| b.contains("pub struct ActionTx"))
        .and_then(|b| b.split("```").next())
        .unwrap_or_else(|| panic!("docs/10 must keep a ```rust block holding the A-08 contract"));
    assert_eq!(
        code_only(block),
        code_only(contract),
        "contracts/tx.rs and docs/10 § The ActionTx contract have diverged"
    );
}

/// The tuple docs/10 and docs/14 both print is the field list this contract implements — a renamed
/// field has to be renamed in the spec in the same commit, or this fails.
#[test]
fn docs10_envelope_tuple_matches_field_order() {
    assert_eq!(FIELD_ORDER.len(), 7);
    for name in FIELD_ORDER {
        assert!(
            !name.is_empty() && !name.contains(' '),
            "{name:?} is not a field name"
        );
    }

    let doc = include_str!("../../docs/10-multiplayer.md");
    assert_eq!(
        tuple_fields(doc, "ActionTx = ⟨"),
        FIELD_ORDER,
        "docs/10's envelope no longer matches contracts/tx.rs"
    );

    // The glossary repeats the tuple; it is the row a reader trusts when the design doc is long.
    let glossary = include_str!("../../docs/14-glossary.md");
    assert_eq!(
        tuple_fields(glossary, "⟨"),
        FIELD_ORDER,
        "docs/14's ActionTx row no longer matches contracts/tx.rs"
    );
}

fn tuple_fields<'a>(text: &'a str, marker: &str) -> Vec<&'a str> {
    let line = text
        .lines()
        .find(|l| l.contains(marker) && l.contains('⟩') && l.contains("Signature"))
        .unwrap_or_else(|| panic!("no ActionTx tuple found by marker {marker:?}"));
    line.split(['⟨', '⟩'])
        .nth(1)
        .expect("tuple delimiters present")
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .collect()
}

fn code_only(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .map(str::to_owned)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect()
}
