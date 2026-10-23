//! `assetc`: authoring tree -> A-07 node records -> `ShapeAsset` bytes -> a loaded tree again.
//!
//! docs/09's gates (AS1-AS5) need a compiler, and the compiler's real work is the first step: child
//! order, `subtree_len`, the parameter pool, and refusing what the grammar or the evaluator cannot
//! honour. `writer::to_bytes` serialises that layout into the envelope `contracts/sdf.fbs` declares,
//! already pinned by `contracts/tests/sdf_schema.rs` and checked against `flatc` in CI, and
//! `reader::from_bytes` is the load side an engine needs - the one that has to accept the bytes
//! *another* implementation wrote.
//!
//! The Rust model rather than RON first, deliberately: parsing is the cheap half and its schema story
//! is OQ-10's; the semantics below are where a wrong choice silently authors a different shape, and
//! they are testable today against `crates/render`'s hand-built trees.

pub mod reader;
pub mod writer;

use contracts::sdf::{
    arity_of, is_known_kind, pack_first_and_flags, pack_kind_and_arity, pack_params_len,
    NodeRecord, FLAG_HAS_MATERIAL, FLAG_IS_ROOT, MAX_TREE_DEPTH,
};
pub mod budget;
pub mod order;

/// The dev-profile reload loop: recompile on change, replace atomically, keep the last good asset on failure.
pub mod watch;

use render::Tree;

/// A shape in the authoring sense: what a content file declares, before any buffer exists.
///
/// `Deserialize` makes a RON file on disk this type, which is doc 09's authoring format and ADR-014's
/// canonical choice with JSON as an alias. Parsing text here is not a hot-path sin: AGENTS.md §3 bans
/// string parsing during gameplay frames and explicitly exempts tools, and this crate is a tool - the
/// engine at runtime only ever sees the compiled bytes this crate writes.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Shape {
    /// Radius then centre. A sphere carries its own centre because the grammar has no translate node,
    /// which is also why `RoundedBox` has no position: it is centred by definition.
    Sphere {
        r: f32,
        centre: [f32; 3],
    },
    RoundedBox {
        half: [f32; 3],
        r: f32,
    },
    Plane {
        normal: [f32; 3],
        offset: f32,
    },
    Union {
        a: Box<Shape>,
        b: Box<Shape>,
    },
    SmoothUnion {
        a: Box<Shape>,
        b: Box<Shape>,
        k: f32,
    },
    /// `kept` minus `cutter` - the same argument order `crates/render` implements as
    /// `max(dKept, -dCutter)`.
    Subtract {
        kept: Box<Shape>,
        cutter: Box<Shape>,
    },
    Intersect {
        a: Box<Shape>,
        b: Box<Shape>,
    },
    Repeat {
        period: f32,
        child: Box<Shape>,
    },
    Mirror {
        axis: u8,
        child: Box<Shape>,
    },
}

/// Full entity wrapper for Phase 2 entity authoring.
///
/// Wraps a Shape with transform and state metadata so RON files can declare complete entities
/// rather than just raw SDF trees. This is doc 16's entity framework integration point.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct EntityDef {
    /// The SDF shape tree
    pub shape: Shape,
    /// Initial position (default: origin)
    #[serde(default = "default_position")]
    pub position: [f32; 3],
    /// Initial rotation as quaternion [x, y, z, w] (default: identity)
    #[serde(default = "default_rotation")]
    pub rotation: [f32; 4],
    /// Uniform scale (default: 1.0)
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// Material slot index into the material registry (default: 0 = pig_skin)
    #[serde(default)]
    pub material_id: u8,
    /// Entity flags: bit 0 = alive, bit 1 = active (default: 0x03 = alive+active)
    #[serde(default = "default_flags")]
    pub flags: u8,
    /// Stack quantity for ledger persistence (default: 0)
    #[serde(default)]
    pub quantity: u8,
}

fn default_position() -> [f32; 3] {
    [0.0, 0.0, 0.0]
}

fn default_rotation() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0] // identity quaternion
}

fn default_scale() -> f32 {
    1.0
}

fn default_flags() -> u8 {
    0x03 // ALIVE | ACTIVE
}

impl Shape {
    /// Nodes in the subtree, so the CLI can refuse a shape whose parameter pool would not fit the
    /// 16-bit index the record format allows - a limit that has to be checked before encoding, not
    /// discovered as truncated offsets afterwards.
    pub fn node_count(&self) -> usize {
        1 + self
            .children()
            .iter()
            .map(|child| child.node_count())
            .sum::<usize>()
    }

    fn children(&self) -> Vec<&Shape> {
        match self {
            Shape::Union { a, b } | Shape::SmoothUnion { a, b, .. } | Shape::Intersect { a, b } => {
                vec![a.as_ref(), b.as_ref()]
            }
            // Named fields here, not `a`/`b`: the kept solid and the cutter are different arguments and
            // a pattern that swaps them would compile happily and cut the wrong half.
            Shape::Subtract { kept, cutter } => vec![kept.as_ref(), cutter.as_ref()],
            Shape::Repeat { child, .. } | Shape::Mirror { child, .. } => vec![child.as_ref()],
            _ => Vec::new(),
        }
    }
}

/// Every way a shape can fail to become an asset, before a byte is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowerError {
    /// Deeper than A-07's draft cap, so a tree the marcher cannot budget is refused by the compiler
    /// rather than discovered on a frame.
    TooDeep { depth: u32 },
    /// An axis outside 0..=2. Refused rather than clamped: clamping silently authors the wrong shape.
    BadAxis,
    /// A non-finite parameter. NaN travels through a distance field into every pixel and raises nothing,
    /// so this is where it dies.
    NonFiniteParameter,
    /// A placement quaternion whose four components do not satisfy `x² + y² + z² + w² = 1`. Distinct from
    /// [`Self::NonFiniteParameter`] on purpose: the finite-but-unnormalised case is the one an author
    /// actually types, and telling them their numbers are NaN is a message that sends them looking for
    /// the wrong mistake.
    NonUnitQuaternion,
    /// A scale of zero or below. The compiler's refusal, not a clamp — a creature flattened to nothing
    /// still has to be *said* to have no inside.
    NonPositiveScale,
    /// A kind the grammar defines but the evaluator refuses, so lowering it would produce an asset
    /// that `render::validate` rejects at load.
    Unlowerable,
}

/// A lowering refusal plus **where in the authored tree** it happened.
///
/// This is doc 09's gate AS3: a schema violation has to fail the build with a message an agent can act on
/// without opening the file and guessing. The parser already supplies byte spans for *syntax* failures
/// (`1:32-1:33: Expected opening \`(\``), but a shape can be perfectly well-formed RON and still be
/// refused - a NaN radius, an axis of 7, a tree past the depth cap - and those failures come out of the
/// walker, which knows the position and would otherwise throw it away.
///
/// `path` is the authored route, not the record index: `Union.a.Mirror` is something an author can find in
/// the file, whereas node 3 is a fact about the compiled output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub error: LowerError,
    pub path: Vec<&'static str>,
}

impl Refusal {
    fn at(path: &[&'static str], error: LowerError) -> Self {
        Refusal {
            error,
            path: path.to_vec(),
        }
    }

    /// The authored location, as it would be typed: `Union.a.Mirror`.
    #[must_use]
    pub fn location(&self) -> String {
        if self.path.is_empty() {
            "the root shape".to_string()
        } else {
            self.path.join(".")
        }
    }
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let here = self.location();
        match &self.error {
            LowerError::TooDeep { depth } => write!(
                f,
                "{here} is {depth} levels deep, past the grammar's cap of {MAX_TREE_DEPTH}"
            ),
            LowerError::BadAxis => write!(f, "{here} has an axis outside 0..=2"),
            LowerError::NonFiniteParameter => {
                write!(f, "{here} carries a non-finite parameter (NaN or infinite)")
            }
            LowerError::NonUnitQuaternion => {
                write!(
                    f,
                    "{here} is not a unit quaternion: x² + y² + z² + w² must equal 1"
                )
            }
            LowerError::NonPositiveScale => {
                write!(
                    f,
                    "{here} must be greater than zero; nothing has an inside at that scale"
                )
            }
            LowerError::Unlowerable => {
                write!(
                    f,
                    "{here} is a kind the evaluator refuses, so no asset can be emitted"
                )
            }
        }
    }
}

/// A lowered asset: a tree the evaluator can run, plus the header fields the envelope needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub tree: Tree,
    /// Nested evaluations, the same count `render::validate` derives independently.
    pub depth: u32,
}

impl Asset {
    /// Whether any node carries the material flag. Nothing sets one yet; the accessor exists so the
    /// writer's `FLAG_HAS_MATERIAL` claim is testable instead of assumed.
    pub fn has_material(&self) -> bool {
        self.tree
            .nodes
            .iter()
            .any(|record| record.flags() & (FLAG_HAS_MATERIAL | FLAG_IS_ROOT) == FLAG_HAS_MATERIAL)
    }
}

/// The author-facing name of a shape, used in refusal paths.
fn label(shape: &Shape) -> &'static str {
    match shape {
        Shape::Sphere { .. } => "Sphere",
        Shape::RoundedBox { .. } => "RoundedBox",
        Shape::Plane { .. } => "Plane",
        Shape::Union { .. } => "Union",
        Shape::SmoothUnion { .. } => "SmoothUnion",
        Shape::Subtract { .. } => "Subtract",
        Shape::Intersect { .. } => "Intersect",
        Shape::Repeat { .. } => "Repeat",
        Shape::Mirror { .. } => "Mirror",
    }
}

/// The authored field a child sits in, so a path reads the way the file is written.
fn edge(shape: &Shape, index: usize) -> &'static str {
    match shape {
        Shape::Subtract { .. } => match index {
            0 => "kept",
            _ => "cutter",
        },
        Shape::Repeat { .. } | Shape::Mirror { .. } => "child",
        _ => match index {
            0 => "a",
            _ => "b",
        },
    }
}

#[inline]
fn kind_of(shape: &Shape) -> u16 {
    use contracts::sdf::*;
    match shape {
        Shape::Sphere { .. } => NODE_SPHERE,
        Shape::RoundedBox { .. } => NODE_ROUNDED_BOX,
        Shape::Plane { .. } => NODE_PLANE,
        Shape::Union { .. } => NODE_UNION,
        Shape::SmoothUnion { .. } => NODE_SMOOTH_UNION,
        Shape::Subtract { .. } => NODE_SUBTRACT,
        Shape::Intersect { .. } => NODE_INTERSECT,
        Shape::Repeat { .. } => NODE_REPEAT,
        Shape::Mirror { .. } => NODE_MIRROR,
    }
}

/// Parameters and children, per shape. Kept beside `kind_of` so the parameter meanings the evaluator
/// documents and the bytes that carry them cannot drift apart quietly.
fn parts<'s>(
    shape: &'s Shape,
    path: &[&'static str],
) -> Result<(Vec<f32>, Vec<&'s Shape>), Refusal> {
    let finite = |values: &[f32]| -> Result<(), Refusal> {
        if values.iter().all(|value| value.is_finite()) {
            Ok(())
        } else {
            Err(Refusal::at(path, LowerError::NonFiniteParameter))
        }
    };
    Ok(match shape {
        Shape::Sphere { r, centre } => {
            finite(&[*r, centre[0], centre[1], centre[2]])?;
            (vec![*r, centre[0], centre[1], centre[2]], Vec::new())
        }
        Shape::RoundedBox { half, r } => {
            finite(&[half[0], half[1], half[2], *r])?;
            (vec![half[0], half[1], half[2], *r], Vec::new())
        }
        Shape::Plane { normal, offset } => {
            finite(&[normal[0], normal[1], normal[2], *offset])?;
            (vec![normal[0], normal[1], normal[2], *offset], Vec::new())
        }
        Shape::Union { a, b } | Shape::Intersect { a, b } => {
            (Vec::new(), vec![a.as_ref(), b.as_ref()])
        }
        Shape::Subtract { kept, cutter } => (Vec::new(), vec![kept.as_ref(), cutter.as_ref()]),
        Shape::SmoothUnion { a, b, k } => {
            finite(&[*k])?;
            (vec![*k], vec![a.as_ref(), b.as_ref()])
        }
        Shape::Repeat { period, child } => {
            finite(&[*period])?;
            (vec![*period], vec![child.as_ref()])
        }
        Shape::Mirror { axis, child } => {
            if *axis > 2 {
                return Err(Refusal::at(path, LowerError::BadAxis));
            }
            (vec![*axis as f32], vec![child.as_ref()])
        }
    })
}

fn depth_of(shape: &Shape, path: &mut Vec<&'static str>) -> Result<u32, Refusal> {
    let (_, kids) = parts(shape, path)?;
    let mut deepest = 0u32;
    for (index, &kid) in kids.iter().enumerate() {
        path.push(edge(shape, index));
        path.push(label(kid));
        deepest = deepest.max(depth_of(kid, path)?);
        path.pop();
        path.pop();
    }
    Ok(1 + deepest)
}

/// Lower into pre-order records: parent first, children left to right, `subtree_len` derived from the
/// children's own spans so the layout is computed rather than asserted.
pub fn lower(root: &Shape) -> Result<Asset, Refusal> {
    let root = &order::rebalance(root);
    let mut path = vec![label(root)];
    let depth = depth_of(root, &mut path)?;
    if depth > MAX_TREE_DEPTH {
        return Err(Refusal::at(&path, LowerError::TooDeep { depth }));
    }
    let kind = kind_of(root);
    if !is_known_kind(kind) || arity_of(kind) == usize::MAX {
        return Err(Refusal::at(&path, LowerError::Unlowerable));
    }
    let mut params: Vec<f32> = Vec::new();
    let mut nodes: Vec<NodeRecord> = Vec::new();
    emit(root, &mut params, &mut nodes, &mut path)?;
    if nodes.is_empty() {
        return Err(Refusal::at(&path, LowerError::Unlowerable));
    }
    nodes[0].0[2] |= (FLAG_IS_ROOT as u32) << 16;
    let tree = Tree { nodes, params };
    render::validate(&tree).map_err(|_| Refusal::at(&path, LowerError::Unlowerable))?;
    Ok(Asset { tree, depth })
}

fn emit(
    shape: &Shape,
    params: &mut Vec<f32>,
    nodes: &mut Vec<NodeRecord>,
    path: &mut Vec<&'static str>,
) -> Result<(), Refusal> {
    let kind = kind_of(shape);
    let arity = arity_of(kind) as u16;
    let (payload, kids) = parts(shape, path)?;
    let first = params.len() as u16;
    params.extend_from_slice(&payload);

    let head = nodes.len();
    nodes.push(NodeRecord([
        pack_kind_and_arity(kind, arity),
        1,
        pack_first_and_flags(first, 0),
        pack_params_len(payload.len() as u16),
    ]));

    let mut span = 1usize;
    for (index, &kid) in kids.iter().enumerate() {
        path.push(edge(shape, index));
        path.push(label(kid));
        emit(kid, params, nodes, path)?;
        path.pop();
        path.pop();
        // The child at `head + span` has finished its own subtree, so its span is final.
        span += nodes[head + span].subtree_len() as usize;
    }
    nodes[head].0[1] = span as u32;
    Ok(())
}

/// Compiled entity with asset and transform metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEntity {
    pub asset: Asset,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
    pub material_id: u8,
    pub flags: u8,
    pub quantity: u8,
}

/// Compile an EntityDef into a ready-to-use entity with lowered SDF tree and metadata.
pub fn compile_entity(def: &EntityDef) -> Result<CompiledEntity, Refusal> {
    let asset = lower(&def.shape)?;

    // A non-finite number is refused before any clause-specific check, because `(NaN - 1.0).abs() > 1e-4`
    // is true and would otherwise report a NaN as "not a unit quaternion" - the wrong mistake, in the
    // wrong words, for the one value an author cannot fix by normalising.
    let rot = def.rotation;
    if !rot.iter().all(|lane| lane.is_finite()) {
        return Err(Refusal::at(&["rotation"], LowerError::NonFiniteParameter));
    }
    let len_sq = rot[0] * rot[0] + rot[1] * rot[1] + rot[2] * rot[2] + rot[3] * rot[3];
    if (len_sq - 1.0).abs() > 1e-4 {
        return Err(Refusal::at(&["rotation"], LowerError::NonUnitQuaternion));
    }

    if !def.scale.is_finite() {
        return Err(Refusal::at(&["scale"], LowerError::NonFiniteParameter));
    }
    if def.scale <= 0.0 {
        return Err(Refusal::at(&["scale"], LowerError::NonPositiveScale));
    }

    // Finiteness, not magnitude: a placement of 1e30 is a real distance and renders as sky, which is
    // how doc 04's unbounded regions and OQ-58's range question are still open. A NaN or an infinity
    // is neither — it is a lane no distance can be computed from, and the march answers it with a
    // frame of sky and its step budget spent.
    if !def.position.iter().all(|lane| lane.is_finite()) {
        return Err(Refusal::at(&["position"], LowerError::NonFiniteParameter));
    }

    Ok(CompiledEntity {
        asset,
        position: def.position,
        rotation: def.rotation,
        scale: def.scale,
        material_id: def.material_id,
        flags: def.flags,
        quantity: def.quantity,
    })
}
