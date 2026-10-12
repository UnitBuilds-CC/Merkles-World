// contracts/sdf.rs — artifact A-07, DRAFT 2026-10-12: the shape/material node grammar.
//
// ADR-012 decided content is data rather than source: one master WGSL shader interprets a shape tree,
// so agents emit small declarative files instead of hand-written shaders, and it named the consequence
// itself — "the grammar is a public API surface; it lives in `contracts/`, versioned". This file is
// that surface. It is DRAFT and needs ADR-031's sign-off before `assetc` or the interpreter treat it
// as frozen fact.
//
// Note on form: this contract is deliberately free of fenced code blocks in its comments, because
// docs/03 mirrors A-10 verbatim inside a markdown fence and a contract containing fences unbalances
// the document the docs checker reads. The interpreter signature is stated as indented lines instead.
//
// What is pinned, and why each is load-bearing:
//
// - **A 16-byte node record, four words, one field place each.** The tree is uploaded as one WGSL
//   storage buffer, so the stride is what the GPU agrees with. The first draft of this layout packed
//   `params_first`, `params_len` and `flags` into a single word, and `flags << 24` silently overwrote
//   the top byte of `params_len`: a lossy writer whose damage shows up only in the reader, which is
//   the one failure mode a record layout must never have. A test caught it; the review did not.
// - **Pre-order plus `subtree_len`.** Indices replace pointers (which do not survive an upload), and a
//   sibling is `index + subtree_len` — arithmetic, not a walk.
// - **A fixed arity per kind, read from this table, never from the file.** The interpreter's branch
//   and `assetc`'s validator consult the same source, so a tree that would make the shader guess is
//   refused before it ships. Combiners are binary here: `assetc` flattens an n-ary authoring union
//   into a left-leaning chain, so one branch is budgeted instead of n.
// - **Geometry values are `f32` and never hashed.** [AGENTS.md](../AGENTS.md) §4 and ADR-017: radii,
//   offsets and blend factors are presentation quantities, and docs/04 § Determinism boundary says the
//   renderer "outputs pixels only". `sin` and `cos` are legal *here* (Twist, Bend, domain warp)
//   precisely because nothing downstream hashes the result. A material id a peer must agree on is a
//   ledger quantity and comes from A-03's records, not from this grammar.
// - **No mesh, no texture, ever.** ADR-011 is identity, not a limitation to ease: a kind that implies
//   an imported asset does not get added.
//
// Not settled here: the FlatBuffer envelope that carries a tree (doc 09, A-07's `.fbs` half — this
// file pins the node record the envelope holds, not the asset header), the material field set
// (`A-04`'s macro fields and the node grammar meet at `NODE_MATERIAL`'s payload, which is OQ-08's),
// and the depth cap, which is a draft number until spike S1 measures a frame-time curve.

/// Grammar format tag. `assetc` writes it next to the FlatBuffer header so an interpreter can refuse a
/// tree it would otherwise misread. A change to the record layout or to any kind's meaning bumps this
/// and is a breaking asset change: every authored tree is recompiled, not reinterpreted.
pub const GRAMMAR_VERSION: u32 = 1;

/// Bytes per node record. Four words; the assert below is what makes the claim checkable.
pub const NODE_STRIDE_BYTES: usize = 16;

/// Ceiling on tree depth, enforced by `assetc` rather than by the shader. **Draft value chosen so that
/// validation exists at all** — spike S1's frame-time curve is what should set it, and docs/04 §
/// Culling and the frame budget is where that measurement lands.
pub const MAX_TREE_DEPTH: u32 = 16;

/// The parameter pool is `f32` bits indexed by a node record; this is the width the index field can
/// reach, stated as a capability rather than a limit anyone has allocated against.
pub const PARAM_POOL_LIMIT: usize = 1 << 16;

// ---------------------------------------------------------------------------
// Kinds — the operator byte of every record. Public numbering: a renumbering silently
// reinterprets every asset on disk, so values are never reused or reshuffled.
// ---------------------------------------------------------------------------

pub const NODE_SPHERE: u16 = 0;
/// Half-extent plus fillet radius — the source's own rounded-cuboid idiom (GS 1002).
pub const NODE_ROUNDED_BOX: u16 = 1;
pub const NODE_CYLINDER: u16 = 2;
pub const NODE_TORUS: u16 = 3;
/// An infinite plane: without it no terrain-shaped surface is expressible, and the alternative was
/// a mesh, which ADR-011 forbids.
pub const NODE_PLANE: u16 = 4;

pub const NODE_UNION: u16 = 8;
/// `k`, the blend radius, is the parameter that changes the field's Lipschitz constant — which is why
/// docs/04 § The march needs it budget-checked rather than assumed: the step guarantee holds only as
/// long as the field stays 1-Lipschitz.
pub const NODE_SMOOTH_UNION: u16 = 9;
pub const NODE_SUBTRACT: u16 = 10;
pub const NODE_INTERSECT: u16 = 11;

pub const NODE_REPEAT: u16 = 16;
pub const NODE_MIRROR: u16 = 17;
/// Transcendental inside the renderer, legal because nothing hashes it.
pub const NODE_TWIST: u16 = 18;
pub const NODE_BEND: u16 = 19;
/// Domain warp over the integer noise lattice (docs/02 § `lattice_hash`): the same hash the engine
/// already uses, so a mod's noise and the engine's agree. Its *inputs* are integer seeds; the output
/// is geometry and is never hashed.
pub const NODE_NOISE: u16 = 20;

/// A material node. Allowed only in a material slot, never inside the shape tree — the split is what
/// keeps "what is this surface" out of "where is the boundary", which doc 04's budget depends on.
pub const NODE_MATERIAL: u16 = 24;

/// Children a kind must have. Unknown or unassigned kinds get `usize::MAX`, which no well-formed tree
/// can satisfy, so a validator refuses them without a second table.
#[inline]
pub const fn arity_of(kind: u16) -> usize {
    match kind {
        NODE_SPHERE | NODE_ROUNDED_BOX | NODE_CYLINDER | NODE_TORUS | NODE_PLANE
        | NODE_MATERIAL => 0,
        NODE_UNION | NODE_SMOOTH_UNION | NODE_SUBTRACT | NODE_INTERSECT => 2,
        NODE_REPEAT | NODE_MIRROR | NODE_TWIST | NODE_BEND | NODE_NOISE => 1,
        _ => usize::MAX,
    }
}

/// Every kind the grammar defines, in one list so the tests can check uniqueness and arity coverage
/// without restating the numbers a third time.
pub const KNOWN_KINDS: [u16; 15] = [
    NODE_SPHERE,
    NODE_ROUNDED_BOX,
    NODE_CYLINDER,
    NODE_TORUS,
    NODE_PLANE,
    NODE_UNION,
    NODE_SMOOTH_UNION,
    NODE_SUBTRACT,
    NODE_INTERSECT,
    NODE_REPEAT,
    NODE_MIRROR,
    NODE_TWIST,
    NODE_BEND,
    NODE_NOISE,
    NODE_MATERIAL,
];

#[inline]
pub const fn is_known_kind(kind: u16) -> bool {
    let mut i = 0;
    while i < KNOWN_KINDS.len() {
        if KNOWN_KINDS[i] == kind {
            return true;
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// The record, as the buffer sees it
// ---------------------------------------------------------------------------

/// Flag bits a validator or interpreter needs without re-deriving them from the tree.
pub const FLAG_IS_ROOT: u16 = 1 << 0;
/// The node has a material slot attached. Kept as a bit because the marcher wants to know before it
/// evaluates: an unmaterialised hit is a wasted `material_eval`.
pub const FLAG_HAS_MATERIAL: u16 = 1 << 1;

/// Word 0: kind, then arity.
#[inline]
pub const fn pack_kind_and_arity(kind: u16, arity: u16) -> u32 {
    (kind as u32) | ((arity as u32) << 16)
}

/// Word 2: the parameter pool's first index, then the flags in the high half.
#[inline]
pub const fn pack_first_and_flags(params_first: u16, flags: u16) -> u32 {
    (params_first as u32) | ((flags as u32) << 16)
}

/// Word 3: the parameter count in the low half; the high half is reserved and stays zero until a
/// revision has a use for it, so an old reader cannot misread a new writer by accident.
#[inline]
pub const fn pack_params_len(params_len: u16) -> u32 {
    params_len as u32
}

const _: () = assert!(core::mem::size_of::<NodeRecord>() == NODE_STRIDE_BYTES);
const _: () = assert!(arity_of(NODE_SPHERE) == 0 && arity_of(NODE_UNION) == 2);
const _: () = assert!(KNOWN_KINDS.len() == 15);
const _: () = assert!(MAX_TREE_DEPTH > 0 && MAX_TREE_DEPTH < 64);

/// One node. The interpreter's signature over this record is the WGSL contract, stated here so a
/// binding and a shader cannot drift into two different interpreters:
///
/// `fn sdf_eval(tree: &[NodeRecord], params: &[f32], node: u32, p: vec3<f32>) -> f32`, and
/// `fn material_eval(tree, params, node: u32, p: vec3<f32>, normal: vec3<f32>) -> Material`.
///
/// Written as inline code on purpose: an indented block in a doc comment is a doctest to
/// rustdoc, and WGSL is not Rust - the first draft of this line broke `cargo test` for a
/// contract that cannot compile. The same reason keeps fenced blocks out of this file: docs/04
/// cannot mirror a contract whose contents unbalance its markdown.
///
/// Both take a node *index*, because the tree lives in one buffer and the walk is index arithmetic.
/// `sdf_eval` returns the signed distance in world units and is the only function the marcher may call
/// per step; `material_eval` runs once at the hit. Neither returns anything a peer hashes, and neither
/// accepts a value smuggled in from the state side ([AGENTS.md](../AGENTS.md) §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRecord(
    /// `[kind|arity, subtree_len, params_first|flags, params_len|reserved]`
    pub [u32; 4],
);

impl NodeRecord {
    #[inline]
    pub const fn kind(&self) -> u16 {
        (self.0[0] & 0xffff) as u16
    }

    #[inline]
    pub const fn arity(&self) -> u16 {
        (self.0[0] >> 16) as u16
    }

    /// Nodes this subtree covers, including itself.
    #[inline]
    pub const fn subtree_len(&self) -> u32 {
        self.0[1]
    }

    #[inline]
    pub const fn params_first(&self) -> u16 {
        (self.0[2] & 0xffff) as u16
    }

    #[inline]
    pub const fn flags(&self) -> u16 {
        (self.0[2] >> 16) as u16
    }

    #[inline]
    pub const fn params_len(&self) -> u16 {
        (self.0[3] & 0xffff) as u16
    }

    /// The first sibling after this subtree. Pre-order plus `subtree_len` makes traversal arithmetic —
    /// the property that lets the interpreter walk a deep tree without a stack of its own.
    #[inline]
    pub const fn next_sibling(&self, index: u32) -> u32 {
        index + self.subtree_len()
    }

    /// Whether the record's own arity claim matches the kind table. `assetc` validates with this and
    /// refuses the asset; the interpreter may then trust the buffer instead of re-checking it per node.
    #[inline]
    pub const fn arity_matches_the_table(&self) -> bool {
        arity_of(self.kind()) as u32 == self.arity() as u32
    }
}
