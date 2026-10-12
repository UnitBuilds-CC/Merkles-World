// A-07's master interpreter, first draft: one program that walks the compiled node array rather than
// one program per entity (ADR-012).
//
// Why this is a loop with an explicit stack and not a recursive function: WGSL has no recursion. That
// is not a detail to route around, it is the reason the two implementations can disagree while both
// look correct - the CPU reference in crates/render recurses, so "the shader agrees with the
// reference" is a claim that has to be *tested*, and the failure mode is a wrong picture rather than a
// compile error. tests/wgsl_matches_cpu.rs is the point of this file.
//
// Field order is contracts::sdf::NodeRecord's word for word, and parameters are read through the same
// params_first/params_len pair, so the storage buffer an `assetc` run produces is the buffer this
// reads - which is docs/09's zero-copy claim checked on a device.

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
const NODE_PLANE: u32 = 4u;
const NODE_UNION: u32 = 8u;
const NODE_SMOOTH_UNION: u32 = 9u;
const NODE_SUBTRACT: u32 = 10u;
const NODE_INTERSECT: u32 = 11u;

// The distance a depth guard hands back, matching the CPU reference's f32::MAX so an unimplemented
// kind is obvious in a comparison instead of plausible.
const UNSUPPORTED: f32 = 3.402823466e+38;

@group(0) @binding(0) var<storage, read> nodes: array<Node>;
@group(0) @binding(1) var<storage, read> pool: array<f32>;
@group(0) @binding(2) var<storage, read> queries: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> header: Header;
@group(0) @binding(4) var<storage, read_write> distances: array<f32>;

fn param(node: Node, offset: u32) -> f32 {
    return pool[(node.first_and_flags & 0xffffu) + offset];
}

fn kind_of(node: Node) -> u32 {
    return node.kind_and_arity & 0xffffu;
}

fn arity_of(node: Node) -> u32 {
    return node.kind_and_arity >> 16u;
}

// Leaf formulas transcribed from crates/render's eval_at, expression by expression. The rounded box
// in particular nests max/min in an order that reads as interchangeable and is not: it is the exact
// distance outside the corner and the linear extension inside it, which is what makes the march's
// "a step is a guaranteed-empty sphere" argument hold at all.
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
    if kind == NODE_PLANE {
        let normal = vec3(param(node, 0u), param(node, 1u), param(node, 2u));
        return dot(p, normal) - param(node, 3u);
    }
    return UNSUPPORTED;
}

fn combine(kind: u32, a: f32, b: f32, node: Node) -> f32 {
    if kind == NODE_UNION {
        return min(a, b);
    }
    if kind == NODE_SMOOTH_UNION {
        // The same cubic smin as the CPU reference: k is the parameter that changes the field's
        // Lipschitz constant, so the two implementations disagreeing here would change the march's
        // step guarantee, not just its shading.
        let k = param(node, 0u);
        let h = max(k - abs(a - b), 0.0) / k;
        return min(a, b) - h * h * h * k * (1.0 / 6.0);
    }
    if kind == NODE_SUBTRACT {
        return max(a, -b);
    }
    if kind == NODE_INTERSECT {
        return max(a, b);
    }
    return UNSUPPORTED;
}

const DEPTH: u32 = 32u;

// Post-order walk of a pre-order array. `subtree_len` is what makes this possible without pointers:
// the first child of a node is the next record, and the second child sits one whole subtree later.
fn eval_from(root: u32, p: vec3<f32>) -> f32 {
    var idx: array<u32, DEPTH>;
    // 0 = enter, 1 = left subtree done, 2 = right subtree done
    var stage: array<u32, DEPTH>;
    var lval: array<f32, DEPTH>;
    var sp: u32 = 0u;
    idx[0] = root;
    stage[0] = 0u;
    var value: f32 = 0.0;

    loop {
        // Descend to the next leaf, remembering the path.
        loop {
            if sp >= header.stack_depth {
                return UNSUPPORTED;
            }
            let node = nodes[idx[sp]];
            if arity_of(node) == 0u {
                value = leaf_distance(node, p);
                break;
            }
            if stage[sp] == 0u {
                stage[sp] = 1u;
                let first = idx[sp] + 1u;
                sp = sp + 1u;
                idx[sp] = first;
                stage[sp] = 0u;
                continue;
            }
            stage[sp] = 2u;
            let first = idx[sp] + 1u;
            let second = first + nodes[first].subtree_len;
            sp = sp + 1u;
            idx[sp] = second;
            stage[sp] = 0u;
            continue;
        }

        // Unwind: hand the value to the parent, either descending into its right child or combining.
        loop {
            if sp == 0u {
                return value;
            }
            sp = sp - 1u;
            let parent_node = nodes[idx[sp]];
            if stage[sp] == 1u {
                lval[sp] = value;
                stage[sp] = 2u;
                let first = idx[sp] + 1u;
                let second = first + nodes[first].subtree_len;
                sp = sp + 1u;
                idx[sp] = second;
                stage[sp] = 0u;
                break;
            }
            value = combine(kind_of(parent_node), lval[sp], value, parent_node);
        }
    }

    // Reached only if the loops above exit without returning. The value is then whatever the
    // last frame produced, and the sentinel is what a caller should see if a tree outran the stack.
    return UNSUPPORTED;
}


// One workgroup item per query. `queries[i]` is (x, y, z, root_index), the root index carried per
// query so a caller can ask about a subtree without a second pipeline.
@compute @workgroup_size(64)
fn evaluate_points(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index >= header.point_count {
        return;
    }
    let query = queries[index];
    distances[index] = eval_from(u32(query.w), query.xyz);
}
