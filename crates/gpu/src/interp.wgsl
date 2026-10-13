// A-07's master interpreter: one program that walks the compiled node array rather than one program per
// entity (ADR-012), covering every kind crates/render's CPU reference implements.
//
// Why this is a loop with an explicit stack and not a recursive function: WGSL has no recursion. That is
// not a detail to route around - it is the reason "the shader agrees with the interpreter" has to be
// *tested*. Two implementations of one grammar, each reviewed alone, can compute different fields. They
// did (see the smooth-union note in combine), and the divergence was 4% of a voxel.
//
// Field order is contracts::sdf::NodeRecord's word for word and parameters come through the same
// params_first/params_len pair, so the storage buffer an `assetc` run writes is the buffer this binds:
// docs/09's zero-copy claim, checked on a device.
//
// Every frame carries its own point. Not decoration: `repeat`, `mirror`, `twist` and `bend` are unary
// nodes that fold *space* before their child is evaluated, so one point per traversal cannot express
// them. The reference's recursion gets a fresh `p` per frame; the stack machine must match that or it
// evaluates a different shape while looking entirely correct.

struct Node {
    kind_and_arity: u32,
    subtree_len: u32,
    first_and_flags: u32,
    params_len: u32,
}

struct Header {
    point_count: u32,
    stack_depth: u32,
    _pad: u32,
    _pad2: u32,
}

// A-07's kind numbers as literals, because renumbering silently reinterprets every asset on disk.
const NODE_SPHERE: u32 = 0u;
const NODE_ROUNDED_BOX: u32 = 1u;
const NODE_CYLINDER: u32 = 2u;
const NODE_TORUS: u32 = 3u;
const NODE_PLANE: u32 = 4u;
const NODE_UNION: u32 = 8u;
const NODE_SMOOTH_UNION: u32 = 9u;
const NODE_SUBTRACT: u32 = 10u;
const NODE_INTERSECT: u32 = 11u;
const NODE_REPEAT: u32 = 16u;
const NODE_MIRROR: u32 = 17u;
const NODE_TWIST: u32 = 18u;
const NODE_BEND: u32 = 19u;

// The sentinel the CPU evaluator's recursion guard returns, so a comparison reports "no surface" instead
// of inventing one.
const UNSUPPORTED: f32 = 3.402823466e+38;

@group(0) @binding(0) var<storage, read> nodes: array<Node>;
@group(0) @binding(1) var<storage, read> pool: array<f32>;
@group(0) @binding(2) var<storage, read> queries: array<vec4<f32>>;
// The header is a *uniform*, not a fifth storage buffer: Limits::downlevel_defaults() allows four
// storage buffers per stage, and asking for five is a validation error on anything that has to live
// downlevel - which includes the WebGL-class backend a browser target would use. Found by running this,
// not by reading a limits table.
@group(0) @binding(3) var<uniform> header: Header;
@group(0) @binding(4) var<storage, read_write> distances: array<f32>;

const DEPTH: u32 = 32u;

struct Frame {
    node: u32,
    // 0 = not entered, 1 = left child delivered, 2 = right child delivered
    stage: u32,
    p: vec3<f32>,
    left: f32,
}

fn param(node: Node, offset: u32) -> f32 {
    return pool[(node.first_and_flags & 0xffffu) + offset];
}

fn kind_of(node: Node) -> u32 {
    return node.kind_and_arity & 0xffffu;
}

fn arity_of(node: Node) -> u32 {
    return node.kind_and_arity >> 16u;
}

fn axis_of(component: f32) -> u32 {
    // `(params[n] as u8).min(2)` in the reference: an out-of-range axis is clamped, not trusted.
    return min(u32(component), 2u);
}

// The reference's `perpendicular`, ordering kept exactly: (1,2) / (0,2) / (0,1). "Rotate the other two"
// is a different linear map depending on which term carries which, and crates/render has a test that
// fails if the pair is swapped - a twin picking the other pair would evaluate a valid-looking field that
// disagrees node-deep.
fn perp_a(axis: u32) -> u32 {
    if axis == 0u {
        return 1u;
    }
    return 0u;
}

fn perp_b(axis: u32) -> u32 {
    if axis == 2u {
        return 1u;
    }
    return 2u;
}

fn component(v: vec3<f32>, index: u32) -> f32 {
    if index == 0u {
        return v.x;
    }
    if index == 1u {
        return v.y;
    }
    return v.z;
}

fn with_component(v: vec3<f32>, index: u32, value: f32) -> vec3<f32> {
    if index == 0u {
        return vec3<f32>(value, v.y, v.z);
    }
    if index == 1u {
        return vec3<f32>(v.x, value, v.z);
    }
    return vec3<f32>(v.x, v.y, value);
}

// Mirrors crates/render's `wrap`: a truncating remainder, then a shift back into [-half, half). WGSL's
// `%` on floats is the operation Rust's `%` is, which is what makes the transcription comparable at all;
// a floor-mod here would put every repeat tile half a period out and still look right.
fn wrap(v: f32, period: f32) -> f32 {
    if period <= 0.0 {
        return v;
    }
    let half = period * 0.5;
    var folded = (v + half) % period;
    if folded < 0.0 {
        folded = folded + period;
    }
    return folded - half;
}

// Unary nodes fold space before their child is evaluated. Each arm is the reference's expression in the
// same order, including the signs the grammar deliberately leaves open: `bend` shears by
// `-amount * p[axis]`, and a second implementation quietly choosing the other sign is the fork this
// contract exists to prevent.
fn map_point(kind: u32, p: vec3<f32>, node: Node) -> vec3<f32> {
    if kind == NODE_REPEAT {
        let period = param(node, 0u);
        return vec3<f32>(wrap(p.x, period), wrap(p.y, period), wrap(p.z, period));
    }
    if kind == NODE_MIRROR {
        let axis = axis_of(param(node, 0u));
        return with_component(p, axis, abs(component(p, axis)));
    }
    if kind == NODE_TWIST {
        // cos/sin are legal here and nowhere near a digest (docs/02, AGENTS.md §4). They are also the one
        // place a GPU vendor may legitimately differ from libm, so the agreement example measures that
        // distance rather than assuming it is zero.
        let axis = axis_of(param(node, 1u));
        let a = perp_a(axis);
        let b = perp_b(axis);
        let angle = param(node, 0u) * component(p, axis);
        let pa = component(p, a);
        let pb = component(p, b);
        let rotated_a = cos(angle) * pa - sin(angle) * pb;
        let rotated_b = sin(angle) * pa + cos(angle) * pb;
        return with_component(with_component(p, a, rotated_a), b, rotated_b);
    }
    if kind == NODE_BEND {
        let axis = axis_of(param(node, 1u));
        let b = perp_b(axis);
        let sheared = component(p, b) - param(node, 0u) * component(p, axis);
        return with_component(p, b, sheared);
    }
    return p;
}

// Leaf formulas transcribed from crates/render's eval_at, expression by expression. The rounded box
// nests max/min in an order that reads as interchangeable and is not: exact distance outside the corner,
// the linear extension inside it, which is what makes a march step a guaranteed-empty sphere.
fn leaf_distance(node: Node, p: vec3<f32>) -> f32 {
    let kind = kind_of(node);
    if kind == NODE_SPHERE {
        let centre = vec3(param(node, 1u), param(node, 2u), param(node, 3u));
        return length(p - centre) - param(node, 0u);
    }
    if kind == NODE_ROUNDED_BOX {
        let half = vec3(param(node, 0u), param(node, 1u), param(node, 2u));
        let q = abs(p) - half;
        let outside = length(max(q, vec3(0.0)));
        return outside + min(max(max(q.x, q.y), q.z), 0.0) - param(node, 3u);
    }
    if kind == NODE_CYLINDER {
        let axis = axis_of(param(node, 2u));
        let a = perp_a(axis);
        let b = perp_b(axis);
        let radial = length(vec3(component(p, a), component(p, b), 0.0)) - param(node, 0u);
        let cap = abs(component(p, axis)) - param(node, 1u);
        let outside = length(max(vec3(radial, cap, 0.0), vec3(0.0)));
        return outside + min(max(radial, cap), 0.0);
    }
    if kind == NODE_TORUS {
        // Two 3-vector lengths: to the centre circle, then along the minor radius. The reference's first
        // draft handed `length` a two-element array; the equivalent trap here is the axis slots, so the
        // ring stays in x-z with the tube measured through y.
        let to_circle = length(vec3(p.x, 0.0, p.z)) - param(node, 0u);
        return length(vec3(to_circle, p.y, 0.0)) - param(node, 1u);
    }
    if kind == NODE_PLANE {
        let normal = vec3(param(node, 0u), param(node, 1u), param(node, 2u));
        return dot(p, normal) - param(node, 3u);
    }
    // Noise (needs docs/02's integer lattice hash plus OQ-45's payload meaning) and a material node in
    // the shape path are refused exactly as the reference refuses them: no plausible distance for a
    // question nobody has answered.
    return UNSUPPORTED;
}

fn combine(kind: u32, a: f32, b: f32, node: Node) -> f32 {
    if kind == NODE_UNION {
        return min(a, b);
    }
    if kind == NODE_SMOOTH_UNION {
        // Mirrored from crates/render::smin: quadratic, factor 0.25, and k <= 0 collapses to the plain
        // min. The first draft here used the literature's cubic form (h^3, k/6) - also a valid smooth
        // minimum, also 1-Lipschitz - and the two implementations differed by 0.042 world units, about
        // 4% of a voxel, inside the blend region. Both look right; only one is the contract's.
        let k = param(node, 0u);
        if k <= 0.0 {
            return min(a, b);
        }
        let h = max(1.0 - abs(b - a) / k, 0.0);
        return min(a, b) - h * h * k * 0.25;
    }
    if kind == NODE_SUBTRACT {
        return max(a, -b);
    }
    if kind == NODE_INTERSECT {
        return max(a, b);
    }
    return UNSUPPORTED;
}

fn first_child(index: u32) -> u32 {
    return index + 1u;
}

fn second_child(index: u32) -> u32 {
    let first = index + 1u;
    return first + nodes[first].subtree_len;
}

// Post-order walk of a pre-order array. `subtree_len` is what makes it pointer-free: a node's first child
// is the next record, and its second child sits one whole subtree later.
fn eval_from(root: u32, origin: vec3<f32>) -> f32 {
    var stack: array<Frame, DEPTH>;
    stack[0] = Frame(root, 0u, origin, 0.0);
    var sp: u32 = 0u;
    var value: f32 = 0.0;
    var descending = true;

    loop {
        if descending {
            loop {
                if sp >= header.stack_depth || sp >= DEPTH {
                    return UNSUPPORTED;
                }
                let frame = stack[sp];
                let node = nodes[frame.node];
                if arity_of(node) == 0u {
                    value = leaf_distance(node, frame.p);
                    descending = false;
                    break;
                }
                // A frame is only ever entered at stage 0 - the unwind path pushes the second child
                // itself - so any other stage here means the walk lost its place. Refuse rather than
                // evaluate a subtree twice and return something plausible.
                if frame.stage != 0u {
                    return UNSUPPORTED;
                }
                stack[sp].stage = 1u;
                let child = first_child(frame.node);
                sp = sp + 1u;
                stack[sp] = Frame(child, 0u, map_point(kind_of(node), frame.p, node), 0.0);
                continue;
            }
        } else {
            loop {
                if sp == 0u {
                    return value;
                }
                sp = sp - 1u;
                let parent_frame = stack[sp];
                let parent = nodes[parent_frame.node];
                if arity_of(parent) == 1u {
                    // A unary node's value is its child's value at a folded point: nothing to combine,
                    // so keep unwinding with the same number.
                    continue;
                }
                if parent_frame.stage == 1u {
                    stack[sp].left = value;
                    stack[sp].stage = 2u;
                    let child = second_child(parent_frame.node);
                    sp = sp + 1u;
                    stack[sp] = Frame(child, 0u, parent_frame.p, 0.0);
                    descending = true;
                    break;
                }
                value = combine(kind_of(parent), parent_frame.left, value, parent);
            }
        }
    }

    // Unreachable: both inner loops end in `return` or in a `break` back into the other phase. naga
    // still requires every path out of a function to return, and an explicit sentinel here is the
    // honest answer - a walk that somehow exits its own loop has lost a subtree, and a lost subtree
    // should read as "no surface", not as whatever `value` happened to hold.
    return UNSUPPORTED;
}

// One workgroup item per query. `queries[i]` is (x, y, z, root index), so a caller can ask about a
// subtree without a second pipeline.
@compute @workgroup_size(64)
fn evaluate_points(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index >= header.point_count {
        return;
    }
    let query = queries[index];
    distances[index] = eval_from(u32(query.w), query.xyz);
}
