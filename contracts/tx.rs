// contracts/tx.rs — artifact A-08, DRAFT 2026-10-11: the `ActionTx` envelope, the single unit of
// multiplayer traffic (ADR-021, docs/10). ADR-029 is what unblocked this file too: a transaction
// that mutates a slab carries an A-03 record, and until the record had a byte layout the envelope
// had nothing to carry.
//
// What this file decides, and what it does not:
// - decided: the seven fields and their order (docs/10 writes the tuple; `FIELD_ORDER` pins it),
//   the three payload classes and their stable wire tags, the split of address half from value
//   half, the transport class each kind belongs to (ADR-022), and the causal-order comparison.
// - not decided: the **byte** layout. That is ADR-015's FlatBuffers transcription, gated on OQ-30
//   (`.fbs` versioning) and OQ-23 (what persists). The integers below are field widths and tags,
//   not offsets, and no code here claims a wire size.
// - not decided: identities and trust — who issues a `PeerId`/`ActorId`/`EntityId`/`ModuleId`, how
//   wide they really are, and what the signature is (OQ-46, with OQ-11 upstream). Each carries a
//   `// pending` note, and the `u32`s are draft carriers of the same kind A-09 uses for `ModuleID`,
//   not conclusions.
//
// Rules this contract keeps:
// - **Tags are literals, never declaration order** (the A-09 rule, same reason): inserting a kind
//   must not renumber its neighbours, because peers compare tags across a version boundary.
// - **Only kinematics are floats** (AGENTS.md §4). A root must never depend on one, and docs/10 is
//   explicit that kinematics are transient and never hashed — so the `f32`s below are legal
//   precisely because nothing digests them. A future variant that hashes a float is the bug to
//   watch for here, not a signature.
// - **No allocation, core-only.** Payloads borrow; the envelope is a stack value. Same wasm32
//   guard as the rest of `contracts/`.
//
// This file is mirrored by docs/10-multiplayer.md § The ActionTx contract; the
// `tx_contract_matches_spec_document` test in contracts/tests fails if the two ever diverge.

use crate::coords::LocalOffset;
use crate::delta::{DeltaRecord, RegionSlot};

/// The envelope, in the order docs/10 and ADR-021 write it. A mirror target, not decoration: the
/// test that reads docs/10's tuple compares against this list, so a renamed field has to be
/// renamed in the spec in the same commit.
pub const FIELD_ORDER: [&str; 7] = [
    "SequenceID",
    "ActorID",
    "Kind",
    "Target",
    "Payload",
    "TickEpoch",
    "Signature",
];

/// A hybrid-logical-clock reading — the first half of `SequenceID`, and the staleness bound a
/// client may never assume it can roll back past (docs/10 consistency rule 2).
// pending OQ-46: the clock's physical meaning (what it counts against, and who may advance it) is
// the identity-and-trust question, not this file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hlc(pub u64);

/// The connection that sent the transaction. Distinct from [`ActorId`] because one peer may
/// control several actors, and a server relaying on a client's behalf is a third case OQ-46 owns.
// pending OQ-46: width is a draft carrier; the key material that binds it to a signature is the
// open half of the same question (OQ-11 upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerId(pub u32);

/// The in-world agent the transaction acts as.
// pending OQ-46, as for `PeerId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorId(pub u32);

/// An entity in the kinematics class. Entity membership itself is a CRDT set (docs/10), so this
/// names one; it does not own one.
// pending OQ-46 for the width, and OQ-35 for what happens when two peers write one entity field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityId(pub u32);

/// A mod module, for the signal-trigger class.
// pending OQ-46 for the width, A-02 for what a module may be signalled at all, and OQ-11 for how a
// module id is established between peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleId(pub u32);

/// Which exported function of [`ModuleId`] fires — `FunctionIndex` in docs/10's table, an index
/// into the module's WIT export list rather than a name, because a wire format must not carry
/// strings it would then have to resolve (AGENTS.md §3).
// pending OQ-46 / A-02: whether this is an export ordinal or a contract-assigned signal id is
// doc 08's to settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionIndex(pub u32);

/// The epoch a transaction claims to belong to. Without it a receiver cannot validate or roll back
/// (ADR-021 rejects sending deltas bare for exactly that reason).
///
/// This is an epoch ordinal, not a duration: docs/03's Δt has no declared unit yet (OQ-42), and
/// nothing here pretends otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickEpoch(pub u64);

/// Causal order inside one epoch: `(HLC, PeerID)` (docs/10 rule 2, ADR-027).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceId {
    pub hlc: Hlc,
    pub peer: PeerId,
}

impl SequenceId {
    /// Which of two sequence ids sorts first. The peer id breaks ties, because "concurrent" is not
    /// an option in a design that re-applies the pool in one order: two peers at the same reading
    /// still have to agree about which came first, and that agreement is what gate M2 measures.
    /// Equal ids are equal, so this is a total order over distinct pairs and never a coin flip.
    #[inline]
    pub const fn precedes(&self, other: &Self) -> bool {
        self.hlc.0 < other.hlc.0 || (self.hlc.0 == other.hlc.0 && self.peer.0 < other.peer.0)
    }
}

/// Which of docs/10's three payload classes an action belongs to. The explicit discriminants are
/// the wire tags; `UNKNOWN`/0 is reserved so a tag a build does not know cannot alias a real kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    /// Block breaks, placements, terraforming — an A-03 record.
    SlabDeltaMutate = 1,
    /// Player and mob movement — transient, never hashed.
    EntityKinematics = 2,
    /// Switches, inventories, spells, circuit inputs.
    WasmSignalTrigger = 3,
}

impl TxKind {
    /// The stable tag for this kind.
    #[inline]
    pub const fn tag(self) -> u8 {
        match self {
            Self::SlabDeltaMutate => 1,
            Self::EntityKinematics => 2,
            Self::WasmSignalTrigger => 3,
        }
    }

    /// Decode a tag. `None` is the `NetError::ActionTxMalformed` case for a stream from a newer
    /// build, and it is a total function rather than a panic because tags arrive from strangers.
    #[inline]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::SlabDeltaMutate),
            2 => Some(Self::EntityKinematics),
            3 => Some(Self::WasmSignalTrigger),
            _ => None,
        }
    }

    /// Which of ADR-022's two channel classes this kind uses. Kept next to the tag so the
    /// "reliable/ordered for mutations, unreliable for kinematics" rule cannot be re-decided
    /// locally by whichever crate builds a datagram.
    #[inline]
    pub const fn channel(self) -> Channel {
        match self {
            Self::EntityKinematics => Channel::Unordered,
            Self::SlabDeltaMutate | Self::WasmSignalTrigger => Channel::Ordered,
        }
    }
}

/// ADR-022's channel classes. A dropped position frame is superseded by the next; a dropped break
/// or circuit trigger is a desync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Reliable, ordered — slab mutations and WASM signals.
    Ordered,
    /// Unreliable, unordered — kinematics.
    Unordered,
}

/// What the action is *aimed at*.
///
/// docs/10's payload table lists each class's whole content, and A-08 splits it the way ADR-015's
/// wire-delta row does: this field is the address half, [`TxPayload`] is the value half. The split
/// is what keeps the kinematics class honest — an entity's identity is stable across frames while
/// its position is thrown away every 50 ms, so they belong in different fields.
///
/// A voxel target is a slot plus an intra-region triple, never a `RegionKey`: the region resolves
/// through the epoch header's slot table on both sides (docs/02, ADR-021), which is the whole
/// reason a delta fits in 8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A single voxel in a region this epoch has a slot for.
    Voxel {
        slot: RegionSlot,
        offset: LocalOffset,
    },
    /// An entity — a player, a mob, anything in the membership set.
    Entity { id: EntityId },
    /// A mod's exported entry point.
    Module {
        module: ModuleId,
        function: FunctionIndex,
    },
}

/// The value half of the action.
#[derive(Debug, Clone, Copy)]
pub enum TxPayload<'a> {
    /// The 12-bit value field of the record named by a [`Target::Voxel`].
    // pending OQ-45: opaque until docs/03 divides the payload field.
    DeltaValue { payload: u16 },
    /// Camera-relative position, velocity and orientation. Floats are legal here and only here,
    /// because nothing hashes them (docs/10: "transient, never hashed"); the camera anchor itself
    /// stays local, which is why these are relative and not global.
    Kinematics {
        position: [f32; 3],
        velocity: [f32; 3],
        orientation: [f32; 4],
    },
    /// Opaque bytes for the module to interpret — the one class whose meaning a mod defines.
    // pending A-02: the WIT surface decides whether this is a byte blob, an argument tuple, or a
    // borrowed region of the module's linear memory.
    Signal { bytes: &'a [u8] },
}

/// The signature over the transaction. Carries no algorithm, no key id and no length, because OQ-46
/// has not chosen them; a fixed `[u8; N]` here would be a fake commitment that a scheme change
/// breaks anyway.
// pending OQ-46 (with OQ-11 for distribution and OQ-14 for what a divergent root costs the
// proposer): docs/10 says the envelope is "dominated by the signature", which is a size claim
// about a field nobody has picked yet.
#[derive(Debug, Clone, Copy)]
pub struct Signature<'a> {
    pub bytes: &'a [u8],
}

impl<'a> Signature<'a> {
    /// An empty signature is not "unsigned but fine": the envelope's anti-spoofing story is the
    /// one field docs/10 refuses to shrink, so a zero-length signature is a decode failure.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// One signed intent. Every client applies it optimistically and validates it later (docs/10 rule
/// 1); nothing about a world's state crosses the wire, only this.
#[derive(Debug, Clone, Copy)]
pub struct ActionTx<'a> {
    /// Causal position inside the epoch, `(HLC, PeerID)`.
    pub sequence: SequenceId,
    /// Who the action acts as in the world.
    pub actor: ActorId,
    /// Which payload class follows. The discriminator a decoder reads before anything else.
    pub kind: TxKind,
    /// The address half.
    pub target: Target,
    /// The value half.
    pub payload: TxPayload<'a>,
    /// The epoch the sender believes it is acting on.
    pub tick_epoch: TickEpoch,
    /// Proof of proposer, over the fields before it.
    pub signature: Signature<'a>,
}

impl<'a> ActionTx<'a> {
    /// Does one transaction describe itself consistently? `Kind` is structurally redundant with
    /// the two enums below — it is the tag a decoder needs *before* parsing them — so the
    /// three must agree or the envelope is a lie about its own shape. The decoder raises
    /// [`crate::errors::NetError::ActionTxMalformed`] on false; an empty signature is malformed for
    /// the same reason.
    #[inline]
    pub fn is_well_formed(&self) -> bool {
        let classes_agree = matches!(
            (self.kind, &self.target, &self.payload),
            (
                TxKind::SlabDeltaMutate,
                Target::Voxel { .. },
                TxPayload::DeltaValue { .. }
            ) | (
                TxKind::EntityKinematics,
                Target::Entity { .. },
                TxPayload::Kinematics { .. }
            ) | (
                TxKind::WasmSignalTrigger,
                Target::Module { .. },
                TxPayload::Signal { .. }
            )
        );
        classes_agree && !self.signature.is_empty()
    }

    /// The A-03 record a slab mutation writes. This is the seam between the two artifacts: the
    /// wire carries an address and a value, the slab stores one word, and ADR-015's wire-delta row
    /// is the reason the two must be the same change. `None` for every other kind.
    #[inline]
    pub fn delta_record(&self) -> Option<DeltaRecord> {
        match (&self.target, &self.payload) {
            (Target::Voxel { slot, offset }, TxPayload::DeltaValue { payload }) => {
                Some(DeltaRecord::new(*slot, *offset, *payload))
            }
            _ => None,
        }
    }

    /// The channel class this transaction must be sent on (ADR-022).
    #[inline]
    pub const fn channel(&self) -> Channel {
        self.kind.channel()
    }
}
