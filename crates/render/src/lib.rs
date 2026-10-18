//! crates/render — A-07's grammar evaluated, validated and marched on the CPU.
//!
//! This is the first code in the render workstream. It is deliberately **not** the WGSL interpreter:
//! the same grammar evaluated on the CPU is what makes docs/04's claims measurable before a shader
//! exists - that a sphere-trace step is really a guaranteed-empty distance, that a tree which would
//! make the shader guess is refused rather than misread, and that zooming in does not run out of
//! geometry (gate R5). When the WGSL half lands it implements `contracts::sdf`'s signature and must
//! agree with this reference; until then, "a rendered procedural scene" is not claimed - an
//! *evaluated* one is, which is a weaker statement and the only honest one available.
//!
//! **Nothing here is ever hashed.** Every value is `f32` and stays presentation-only
//! ([AGENTS.md](../../AGENTS.md) §4, [ADR-017](../../docs/decisions.md)): distances, positions and
//! normals are why docs/04 says the renderer "outputs pixels only", and gate R4's frame hash is
//! informational by policy. That is the argument for `sqrt` and `hypot` appearing below without the
//! ceremony they would need on the state side.
//!
//! Still owed in this workstream, named rather than implied: the WGSL interpreter, and spike S6's
//! headless shader check (OQ-41, which is additionally blocked on the export-lint decision recorded
//! against OQ-41's own row). R1's frame budget cannot be measured here at all: steps per ray are not
//! nanoseconds per frame on a GPU. What landed since the first draft of this file: the envelope doc 09
//! needed (`contracts/sdf.fbs`, written by and read back through `crates/assetc`), and
//! [`raster`] - a CPU probe that turns a tree into a pixel grid, which is geometry evidence and not a
//! frame.

/// Conservative subtree bounds, and the ray early-out that spends zero evaluations on empty space.
pub mod cull;
pub mod raster;
pub mod stream;
pub mod temporal_cache;

use contracts::sdf::{
    NodeRecord, MAX_TREE_DEPTH, NODE_CYLINDER, NODE_INTERSECT, NODE_MIRROR, NODE_PLANE,
    NODE_REPEAT, NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_SUBTRACT, NODE_TORUS,
    NODE_UNION,
};

/// A point or vector in world units. `f32`, presentation-only.
pub type Vec3 = [f32; 3];

/// A root is at index 0 by convention: `validate` requires exactly one `FLAG_IS_ROOT` node, and
/// pre-order makes a root's span the whole buffer, so the flag can only be on record zero.
pub const ROOT: u32 = 0;

/// A shape tree as the interpreter would receive it in one storage buffer: the records
/// `contracts/sdf` describes, plus the `f32` pool they index by `params_first`/`params_len`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    pub nodes: Vec<NodeRecord>,
    pub params: Vec<f32>,
}

impl Tree {
    #[inline]
    fn record(&self, index: usize) -> NodeRecord {
        self.nodes[index]
    }

    /// The parameters a node reads. `validate` proves the slice is in range, so the evaluator indexes
    /// directly rather than re-checking bounds per node on a path that runs once per march step.
    #[inline]
    pub fn params_of(&self, record: NodeRecord) -> &[f32] {
        let first = record.params_first() as usize;
        &self.params[first..first + record.params_len() as usize]
    }
}

/// Why a tree will not be accepted. Every variant is a thing two implementations could otherwise do
/// differently, which is the whole reason the grammar is a contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    /// A kind outside A-07's table. A reserved band is not a typo to interpret.
    UnknownKind(u16),
    /// A kind the grammar defines but this evaluator has not implemented: refused, never evaluated to
    /// a plausible-looking wrong distance.
    UnsupportedKind(u16),
    /// The record's claimed arity disagrees with `arity_of`, so the walker would descend into children
    /// the author did not write.
    ArityMismatch {
        kind: u16,
        claimed: u16,
        expected: u16,
    },
    /// `subtree_len` is not `1 + sum(children)`. The silent one: the tree still evaluates, and
    /// reports a different shape.
    SubtreeLength {
        index: usize,
        claimed: u32,
        actual: u32,
    },
    /// A child runs past the end of the node array.
    Truncated,
    /// A parameter slice outside the pool.
    ParametersOutOfRange { index: usize },
    /// Deeper than `MAX_TREE_DEPTH`, the draft cap that exists so a tree the marcher cannot budget
    /// never reaches a frame.
    TooDeep { depth: u32 },
    /// Zero or several root flags: the marcher needs exactly one place to start.
    RootCount(u32),
}

/// Kinds this evaluator implements. `Twist`, `Bend` and `Noise` are in the grammar and not here yet,
/// and refusing them is the point of `UnsupportedKind`: an unimplemented operator must never answer a
/// distance question with a guess.
pub const IMPLEMENTED_KINDS: [u16; 13] = [
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
];

#[inline]
pub fn is_implemented(kind: u16) -> bool {
    IMPLEMENTED_KINDS.contains(&kind)
}

/// What `assetc` will run before a tree reaches a buffer, executable here so the rules are not
/// descriptions: kind known, kind implemented, arity from the table, `subtree_len` consistent,
/// parameters inside the pool, exactly one root, depth inside the cap.
pub fn validate(tree: &Tree) -> Result<(), ValidationError> {
    let mut roots = 0u32;
    for (index, record) in tree.nodes.iter().enumerate() {
        let kind = record.kind();
        if !contracts::sdf::is_known_kind(kind) {
            return Err(ValidationError::UnknownKind(kind));
        }
        if !is_implemented(kind) {
            return Err(ValidationError::UnsupportedKind(kind));
        }
        if record.flags() & contracts::sdf::FLAG_IS_ROOT != 0 {
            roots += 1;
        }
        let expected = contracts::sdf::arity_of(kind) as u16;
        if record.arity() != expected {
            return Err(ValidationError::ArityMismatch {
                kind,
                claimed: record.arity(),
                expected,
            });
        }
        let first = record.params_first() as usize;
        let len = record.params_len() as usize;
        if first > tree.params.len() || len > tree.params.len() - first {
            return Err(ValidationError::ParametersOutOfRange { index });
        }
        let mut actual: u32 = 1;
        let mut cursor = index + 1;
        for _ in 0..expected {
            match tree.nodes.get(cursor) {
                None => return Err(ValidationError::Truncated),
                Some(child) => {
                    actual += child.subtree_len();
                    cursor += child.subtree_len() as usize;
                }
            }
        }
        if record.subtree_len() != actual {
            return Err(ValidationError::SubtreeLength {
                index,
                claimed: record.subtree_len(),
                actual,
            });
        }
    }
    if roots != 1 {
        return Err(ValidationError::RootCount(roots));
    }
    let root = tree
        .nodes
        .iter()
        .position(|record| record.flags() & contracts::sdf::FLAG_IS_ROOT != 0)
        .unwrap_or(0);
    let depth = depth_of(tree, root);
    if depth > MAX_TREE_DEPTH {
        return Err(ValidationError::TooDeep { depth });
    }
    Ok(())
}

/// The smallest interpreter stack capacity that can evaluate this tree: one frame per nested node, counted from
/// the root the walk actually starts at.
///
/// This is the number to compile `interp.wgsl`'s `DEPTH` from, and the capacity is a correctness contract rather
/// than a tuning knob - a tree deeper than the capacity does not render slowly, it renders **empty**, because the
/// walk's `sp >= DEPTH` guard returns the no-surface sentinel. docs/04's DEPTH ladder is why it is worth deriving
/// per asset: on R1's own 1080p grid one tree walk costs 4.472 ns at the shipped capacity of 32 and 0.295 ns at
/// capacity 2, so a scene that is two frames deep is paying 15x for state it never pushes.
pub fn tree_depth(tree: &Tree) -> u32 {
    let root = tree
        .nodes
        .iter()
        .position(|record| record.flags() & contracts::sdf::FLAG_IS_ROOT != 0)
        .unwrap_or(0);
    depth_of(tree, root)
}

/// Nested evaluations a ray pays for at `index` - what the depth cap is actually about, since the
/// budget is steps per frame rather than records per buffer.
fn depth_of(tree: &Tree, index: usize) -> u32 {
    let record = tree.record(index);
    if record.arity() == 0 {
        return 1;
    }
    let mut deepest = 0u32;
    let mut cursor = index + 1;
    for _ in 0..record.arity() {
        let child = tree.record(cursor);
        deepest = deepest.max(depth_of(tree, cursor));
        cursor += child.subtree_len() as usize;
    }
    1 + deepest
}

// ---------------------------------------------------------------------------
// The evaluator
// ---------------------------------------------------------------------------

#[inline]
pub(crate) fn length(a: Vec3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// docs/04's `SmoothUnion(k)`: `k` is the blend radius. The result stays inside `[min - k, min]`, and
/// the upper bound is what keeps the field 1-Lipschitz, which is what makes a sphere-trace step legal.
#[inline]
fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (1.0 - (b - a).abs() / k).max(0.0);
    a.min(b) - h * h * k * 0.25
}

#[inline]
fn wrap(v: f32, period: f32) -> f32 {
    if period <= 0.0 {
        return v;
    }
    let half = period * 0.5;
    let folded = {
        let f = (v + half) % period;
        if f < 0.0 {
            f + period
        } else {
            f
        }
    };
    folded - half
}

/// `sdf_eval`: the signed distance from `p` to the surface of the subtree rooted at `node`.
///
/// Parameter meanings, since A-07 pins widths and not meanings (that split is deliberate - meanings
/// are what a second implementation would otherwise invent):
///
/// - `NODE_SPHERE` `[r, cx, cy, cz]` - a centre, because a shape is placed by its parameters and the
///   grammar has no translate node.
/// - `NODE_ROUNDED_BOX` `[hx, hy, hz, fillet]` - centred by definition, half-extents.
/// - `NODE_CYLINDER` `[r, half_height, axis]`, `NODE_TORUS` `[major_r, minor_r]` in the x-z plane.
/// - `NODE_PLANE` `[nx, ny, nz, d]` - unit normal as authored.
/// - `NODE_SMOOTH_UNION` `[k]` on the parent of a binary pair.
/// - `NODE_REPEAT` `[period]`, `NODE_MIRROR` `[axis]`.
///
/// CSG ordering, which the first draft of this function got backwards: **the kept solid is the first
/// child.** `SUBTRACT(A, B) = max(dA, -dB)` ("inside A, outside B"); writing it as `max(-dA, dB)`
/// computes B minus A, reads correctly at the surface and inverts everywhere else.
pub fn sdf_eval(tree: &Tree, node: u32, p: Vec3) -> f32 {
    eval_at(tree, node as usize, p, 0, None)
}

/// The same field, with per-node skips licensed by [`cull::SceneBounds`].
///
/// Not a second evaluator: one evaluator with an optional bound table, which is the point. Two
/// implementations of A-07 that agree today are two programs tomorrow (the WGSL reference suite exists
/// because of exactly that risk), so the pruning lives inside `eval_at`'s own union arm and this function
/// differs from [`sdf_eval`] only in the argument it passes. When the bounds decline to license a skip -
/// which is every node below a smooth union, and every unbounded kind - the two answer identically, and
/// `tests/cull.rs` asserts that on scenes built to sit on both sides of that line.
pub fn sdf_eval_bnb(tree: &Tree, bounds: &cull::SceneBounds, node: u32, p: Vec3) -> f32 {
    eval_at(tree, node as usize, p, 0, Some(bounds))
}

fn eval_at(
    tree: &Tree,
    index: usize,
    p: Vec3,
    depth: u32,
    bounds: Option<&cull::SceneBounds>,
) -> f32 {
    // The same belt the evaluator carries: `validate` is the gate, this is the recursion's own, so an
    // unvalidated tree hangs a computation rather than a frame.
    if depth > MAX_TREE_DEPTH + 8 {
        return f32::MAX;
    }
    // One tally per node entered, pruned or not, which is the only unit in which a per-node skip can be
    // priced: the march's step count cannot see it, because a skip returns the same distance.
    if let Some(scene) = bounds {
        scene.count_visit();
    }
    let record = tree.record(index);
    let params = tree.params_of(record);
    // A leaf has no children to look up, and reading `index + 1` on a one-node tree is an index past
    // the end - which is what the first version of this function did.
    let (first, second) = if record.arity() > 0 {
        let child = index + 1;
        (child, child + tree.record(child).subtree_len() as usize)
    } else {
        (index, index)
    };
    match record.kind() {
        NODE_SPHERE => {
            let centre = [p[0] - params[1], p[1] - params[2], p[2] - params[3]];
            length(centre) - params[0]
        }
        NODE_ROUNDED_BOX => {
            let q = [
                p[0].abs() - params[0],
                p[1].abs() - params[1],
                p[2].abs() - params[2],
            ];
            let outside = length([q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)]);
            outside + q[0].max(q[1]).max(q[2]).min(0.0) - params[3]
        }
        NODE_CYLINDER => {
            let axis = (params[2] as u8).min(2) as usize;
            let (a, b) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let radial = length([p[a], p[b], 0.0]) - params[0];
            let cap = p[axis].abs() - params[1];
            let outside = length([radial.max(0.0), cap.max(0.0), 0.0]);
            outside + radial.max(cap).min(0.0)
        }
        NODE_TORUS => {
            // Two 3-vector lengths: to the centre circle, then along the minor radius. The first draft
            // handed `length` a two-element array, and the compiler was the only reviewer that noticed.
            let to_circle = length([p[0], 0.0, p[2]]) - params[0];
            length([to_circle, p[1], 0.0]) - params[1]
        }
        NODE_PLANE => p[0] * params[0] + p[1] * params[1] + p[2] * params[2] - params[3],
        NODE_UNION => {
            let a = eval_at(tree, first, p, depth + 1, bounds);
            // The skip ADR-024 asks for, and the only arm that can take it: a union's field is a `min`, so
            // if the other child's own bound is already farther than `a`, its distance cannot be smaller
            // and evaluating it cannot change the answer. `lower_bound` returns `None` wherever the bound
            // does not license the claim - below a smooth union, inside a deformation, on a plane - and a
            // `None` here means the child is evaluated, exactly as it was before this arm existed.
            if let Some(scene) = bounds {
                if let Some(distance) = scene.lower_bound(second, p) {
                    if distance >= a {
                        return a;
                    }
                }
            }
            let b = eval_at(tree, second, p, depth + 1, bounds);
            a.min(b)
        }
        NODE_SMOOTH_UNION => {
            let a = eval_at(tree, first, p, depth + 1, bounds);
            let k = params[0];
            // The second skip ADR-024 asks for, and the only shape a blend can take. The union arm's test
            // (`bound >= a`) is not enough here, because `smin` is not `min`: a far second child still
            // bends the result through `h`. But `h = max(0, 1 - |b - a| / k)` is exactly zero once the two
            // children are `k` apart, and at that point `smin` *is* `min`, so the blend has nothing left to
            // add and the answer is `a`. The comparison is strict for a reason: `distance >= a + k` in `f32`
            // could sit a rounding step below the true `b - a`, and `h` squared is then tiny but not zero,
            // which is an overestimate of the field where an underestimate is what keeps a march honest.
            // `k.max(0.0)` is the other half: a non-positive strength makes `smin` a plain `min`, and the
            // threshold must then be the union arm's `a`, not `a + k` with a negative `k` pulling it down.
            let slack = a + k.max(0.0);
            if let Some(scene) = bounds {
                if let Some(distance) = scene.lower_bound(second, p) {
                    if distance > slack {
                        return a;
                    }
                }
            }
            let b = eval_at(tree, second, p, depth + 1, bounds);
            smin(a, b, k)
        }
        NODE_SUBTRACT => {
            let kept = eval_at(tree, first, p, depth + 1, bounds);
            let cutter = eval_at(tree, second, p, depth + 1, bounds);
            kept.max(-cutter)
        }
        NODE_INTERSECT => {
            let a = eval_at(tree, first, p, depth + 1, bounds);
            let b = eval_at(tree, second, p, depth + 1, bounds);
            a.max(b)
        }
        NODE_REPEAT => eval_at(
            tree,
            first,
            [
                wrap(p[0], params[0]),
                wrap(p[1], params[0]),
                wrap(p[2], params[0]),
            ],
            depth + 1,
            bounds,
        ),
        NODE_MIRROR => {
            let axis = (params[0] as u8).min(2) as usize;
            let mut mirrored = p;
            mirrored[axis] = mirrored[axis].abs();
            eval_at(tree, first, mirrored, depth + 1, bounds)
        }
        // Twist about `axis`: the perpendicular pair rotates by `amount * p[axis]`. cos/sin are
        // legal here and nowhere near a digest - docs/04 says the renderer outputs pixels only
        // (AGENTS.md section 4, ADR-017).
        NODE_TWIST => {
            let axis = (params[1] as u8).min(2) as usize;
            let (a, b) = perpendicular(axis);
            let angle = params[0] * p[axis];
            let (cos, sin) = (angle.cos(), angle.sin());
            let mut twisted = p;
            twisted[a] = cos * p[a] - sin * p[b];
            twisted[b] = sin * p[a] + cos * p[b];
            eval_at(tree, first, twisted, depth + 1, bounds)
        }
        // Shear, not curve: the second perpendicular coordinate moves by amount * p[axis].
        // Written down because the grammar fixes no sign, and a second implementation quietly
        // picking one is the fork this contract exists to prevent.
        NODE_BEND => {
            let axis = (params[1] as u8).min(2) as usize;
            let (_, b) = perpendicular(axis);
            let mut bent = p;
            bent[b] = p[b] - params[0] * p[axis];
            eval_at(tree, first, bent, depth + 1, bounds)
        }
        // A material leaf carries no boundary. Reaching this arm means a material node sat in the
        // shape path; MAX keeps it out of the march instead of inventing a surface for it.
        _ => f32::MAX,
    }
}

// ---------------------------------------------------------------------------
// The march
// ---------------------------------------------------------------------------

/// Where a marched ray landed, and what it cost in evaluations - the quantity spike S1's curve is
/// about, since a frame budget is spent in steps rather than in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub t: f32,
    pub point: Vec3,
    pub steps: u32,
}

/// Sphere-trace `origin + dir * t` for `t` in `(0, max_t]`, stepping by the distance to the surface.
///
/// The step is legal only because the field is 1-Lipschitz: a sphere of radius `d(p)` around `p`
/// provably contains no surface, which is docs/04's "a guaranteed-empty distance". `smin` preserves
/// that bound (it only ever lowers `min`, by at most `k`, and the envelope is tested), and
/// `Repeat`/`Mirror` are isometries of the folded point.
///
/// `surface` is an input rather than a constant because gate R5 is a claim about 1e-6 world units: a
/// fixed absolute epsilon is exactly what makes a naive marcher stop resolving detail as the camera
/// moves in. At 1e-6 the `f32` ulp is around 1e-12, so an epsilon finer than the representation can
/// never be satisfied and the ray walks past - one concrete reason docs/02 keeps camera-relative
/// coordinates and AGENTS.md §4 keeps floats off the hashed side.
pub fn march(
    tree: &Tree,
    root: u32,
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
    surface: f32,
    max_steps: u32,
) -> Option<Hit> {
    march_with(
        tree,
        root,
        Ray {
            origin,
            dir,
            max_t,
            surface,
            max_steps,
        },
        None,
    )
}

/// The same loop, with ADR-024's hierarchy consulted: a ray is first proved empty (or not) by
/// [`cull::SceneBounds::may_reach_subtree`], and each step may then skip subtrees the bound table licenses
/// through [`sdf_eval_bnb`].
///
/// What it is allowed to change is **nothing about the picture** — same pixels, same depth, same hit count
/// as [`march`] — and `tests/cull.rs` asserts that against the frame rather than against the arithmetic.
/// What it changes is how much of the frame it had to compute to get there.
pub fn march_bnb(tree: &Tree, bounds: &cull::SceneBounds, root: u32, ray: Ray) -> Option<Hit> {
    march_with(tree, root, ray, Some(bounds))
}

/// One ray and the budget it is allowed to spend. A struct rather than five parameters, because the
/// march needs the bound table too and clippy is right that seven loose arguments is a bug waiting for an
/// eighth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
    pub max_t: f32,
    pub surface: f32,
    pub max_steps: u32,
}

fn march_with(tree: &Tree, root: u32, ray: Ray, bounds: Option<&cull::SceneBounds>) -> Option<Hit> {
    let Ray {
        origin,
        dir,
        max_t,
        surface,
        max_steps,
    } = ray;
    let mut t = 0.0f32;
    let mut steps = 0u32;
    while t < max_t && steps < max_steps {
        let point = [
            origin[0] + dir[0] * t,
            origin[1] + dir[1] * t,
            origin[2] + dir[2] * t,
        ];
        let distance = match bounds {
            None => sdf_eval(tree, root, point),
            Some(scene) => sdf_eval_bnb(tree, scene, root, point),
        };
        if distance.abs() < surface {
            return Some(Hit { t, point, steps });
        }
        t += distance.max(surface);
        steps += 1;
    }
    None
}

/// The surface normal at a hit, by tetrahedral gradient: four taps, no centre evaluation, no axis
/// order to disagree about. Presentation quality, not hashed state - so `f32` cancellation at depth
/// is a gate R5 shading question and not a determinism one.
pub fn normal(tree: &Tree, root: u32, point: Vec3, eps: f32) -> Vec3 {
    const TAPS: [[f32; 3]; 4] = [
        [1.0, -1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
    ];
    let mut out = [0.0f32; 3];
    for tap in TAPS.iter() {
        let distance = sdf_eval(
            tree,
            root,
            [
                point[0] + tap[0] * eps,
                point[1] + tap[1] * eps,
                point[2] + tap[2] * eps,
            ],
        );
        out[0] += tap[0] * distance;
        out[1] += tap[1] * distance;
        out[2] += tap[2] * distance;
    }
    let len = length(out);
    if len > 0.0 {
        [out[0] / len, out[1] / len, out[2] / len]
    } else {
        out
    }
}

use contracts::sdf::{NODE_BEND, NODE_TWIST};

/// The two axes perpendicular to `axis`, in a fixed order. The order is contract, not taste:
/// "rotate the other two" is a different shape depending on which is the sine term, and a WGSL
/// interpreter that picked the other way would evaluate a valid-looking field that disagrees with
/// this reference node deep.
#[inline]
fn perpendicular(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}
