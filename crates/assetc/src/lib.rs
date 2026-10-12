//! `assetc`'s first stage: authoring tree -> A-07 node records.
//!
//! docs/09's gates (AS1-AS5) need a compiler, and the compiler's real work is this step: child order,
//! `subtree_len`, the parameter pool, and refusing what the grammar or the evaluator cannot honour.
//! Emitting FlatBuffer **bytes** is a separate stage and is not claimed here - `lower` returns exactly
//! the pre-order `NodeRecord` array and `f32` pool that `contracts/sdf.fbs` carries, so the writer's
//! job is now to serialise a layout already pinned by `contracts/tests/sdf_schema.rs` and CI's flatc
//! job.
//!
//! The Rust model rather than RON first, deliberately: parsing is the cheap half and its schema story
//! is OQ-10's; the semantics below are where a wrong choice silently authors a different shape, and
//! they are testable today against `crates/render`'s hand-built trees.

pub mod writer;

use contracts::sdf::{
    arity_of, is_known_kind, pack_first_and_flags, pack_kind_and_arity, pack_params_len,
    NodeRecord, FLAG_HAS_MATERIAL, FLAG_IS_ROOT, MAX_TREE_DEPTH,
};
use render::Tree;

/// A shape in the authoring sense: what a content file declares, before any buffer exists.
#[derive(Debug, Clone, PartialEq)]
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
    /// A kind the grammar defines but the evaluator refuses, so lowering it would produce an asset
    /// that `render::validate` rejects at load.
    Unlowerable,
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
fn parts(shape: &Shape) -> Result<(Vec<f32>, Vec<&Shape>), LowerError> {
    let finite = |values: &[f32]| -> Result<(), LowerError> {
        if values.iter().all(|value| value.is_finite()) {
            Ok(())
        } else {
            Err(LowerError::NonFiniteParameter)
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
                return Err(LowerError::BadAxis);
            }
            (vec![*axis as f32], vec![child.as_ref()])
        }
    })
}

fn depth_of(shape: &Shape) -> Result<u32, LowerError> {
    let (_, kids) = parts(shape)?;
    let mut deepest = 0u32;
    for kid in kids {
        deepest = deepest.max(depth_of(kid)?);
    }
    Ok(1 + deepest)
}

/// Lower into pre-order records: parent first, children left to right, `subtree_len` derived from the
/// children's own spans so the layout is computed rather than asserted.
pub fn lower(root: &Shape) -> Result<Asset, LowerError> {
    let depth = depth_of(root)?;
    if depth > MAX_TREE_DEPTH {
        return Err(LowerError::TooDeep { depth });
    }
    let kind = kind_of(root);
    if !is_known_kind(kind) || arity_of(kind) == usize::MAX {
        return Err(LowerError::Unlowerable);
    }
    let mut params: Vec<f32> = Vec::new();
    let mut nodes: Vec<NodeRecord> = Vec::new();
    emit(root, &mut params, &mut nodes)?;
    if nodes.is_empty() {
        return Err(LowerError::Unlowerable);
    }
    nodes[0].0[2] |= (FLAG_IS_ROOT as u32) << 16;
    let tree = Tree { nodes, params };
    render::validate(&tree).map_err(|_| LowerError::Unlowerable)?;
    Ok(Asset { tree, depth })
}

fn emit(
    shape: &Shape,
    params: &mut Vec<f32>,
    nodes: &mut Vec<NodeRecord>,
) -> Result<(), LowerError> {
    let kind = kind_of(shape);
    let arity = arity_of(kind) as u16;
    let (payload, kids) = parts(shape)?;
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
    for kid in kids {
        emit(kid, params, nodes)?;
        // The child at `head + span` has finished its own subtree, so its span is final.
        span += nodes[head + span].subtree_len() as usize;
    }
    nodes[head].0[1] = span as u32;
    Ok(())
}
