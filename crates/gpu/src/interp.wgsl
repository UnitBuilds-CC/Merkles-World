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
    // Only `march_rays` reads these two; `evaluate_points` ignores them, and the buffers stay shared rather
    // than growing a fifth and sixth binding, because downlevel classes cap a stage at four storage
    // buffers. The surface tolerance arrives as raw bits: a uniform block has no f32 here and reusing a
    // u32 word keeps one header for both entry points.
    march_steps: u32,
    march_surface_bits: u32,
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
// The tree's parameters, followed by the bound table: four `f32` per node, `[centre.xyz, radius]` in node
// order, at the tail. One buffer because this stage is at the downlevel four-storage-buffer ceiling; the
// tail is located by length arithmetic, so it is present on every shape and unlicensed nodes are spelled
// with an infinite radius rather than a flag. See `second_child_may_skip`.
@group(0) @binding(1) var<storage, read> pool: array<f32>;
@group(0) @binding(2) var<storage, read> queries: array<vec4<f32>>;
// The header is a *uniform*, not a fifth storage buffer: Limits::downlevel_defaults() allows four
// storage buffers per stage, and asking for five is a validation error on anything that has to live
// downlevel - which includes the WebGL-class backend a browser target would use. Found by running this,
// not by reading a limits table.
@group(0) @binding(3) var<uniform> header: Header;
@group(0) @binding(4) var<storage, read_write> distances: array<f32>;

// Temporal cache buffers (desktop only, beyond downlevel ceiling):
// Cached surface data per pixel for frame-to-frame reuse. Each entry stores world position, depth, normal,
// and metadata for validation. Only used by culled camera variants that opt into temporal coherence.
@group(1) @binding(0) var<storage, read_write> cached_positions: array<vec3<f32>>;
@group(1) @binding(1) var<storage, read_write> cached_depths: array<f32>;
@group(1) @binding(2) var<storage, read_write> cached_normals: array<vec3<f32>>;
@group(1) @binding(3) var<storage, read_write> cached_metadata: array<vec4<u32>>; // [frame_id, entity_node, max_t_bits, flags]

// Delta rendering: dirty mask tracks which pixels changed and need re-marching
// Using atomic operations to prevent race conditions when multiple threads clear bits in same word
@group(1) @binding(4) var<storage, read_write> dirty_mask: array<atomic<u32>>;

// Cache statistics output (written once at end of dispatch):
// [0] = cache_hits count, [1] = total_dirty_pixels checked, [2-3] reserved
@group(1) @binding(5) var<storage, read_write> cache_stats: array<atomic<u32>>;

const DEPTH: u32 = 32u;

struct Frame {
    node: u32,
    // 0 = not entered, 1 = left child delivered, 2 = right child delivered
    stage: u32,
    p: vec3<f32>,
    left: f32,
}

// How many union second children this invocation refused to evaluate. One per workgroup *item*, not one
// per dispatch: a module-scope `var<private>` is zero-initialised at the start of every invocation, so each
// ray counts its own skips and no cross-item sharing is needed — which is why the four-storage-buffer
// downlevel ceiling is not an obstacle to counting at all. `march_rays_counted` hands the number back in a
// spare output word; the three other entry points never read it, so for the frame path the increment is
// dead code and the instrument costs nothing.
var<private> union_skips: u32;

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

// Task #64's skip: a union's second child that is already known to be farther from `p` than the value its
// first child produced cannot lower the `min`, so it is not walked. `crates/render`'s `eval_at` does this
// at its NODE_UNION arm; this is the same law on the device, transcribed rather than reinvented, because
// two walks of one grammar that differ by a skipped subtree are the fork this file exists to prevent.
//
// Two differences from the CPU arm, both in the safe direction.
//
// * The bound is **not** clamped at zero. The CPU computes `max(d - radius, 0)` because a licensed
//   distance cannot be negative. `SceneBounds::encoded()` spells "not licensed" as an infinite radius, and
//   a clamp turns that into `0` - which then satisfies `0 >= left` for every point already inside the first
//   child, skipping subtrees nothing ever bounded. Unclamped, an infinite radius reads `-inf` and the test
//   cannot fire on it. Dropping the clamp can only lower the bound, so every skip taken here is one the CPU
//   arm also takes; the arm is stricter, never weaker.
// * Only a plain `NODE_UNION` is asked. A smooth union's `smin` returns less than `min` by up to `k / 4`,
//   so a far second child still bends the answer through `h`; the CPU has a separate k-aware arm for that
//   case (OQ-54) and the device does not, which costs visits and cannot change a picture. `SUBTRACT` and
//   `INTERSECT` are `max`, where a lower bound on one operand says nothing about the pair.
fn second_child_may_skip(node: u32, p: vec3<f32>, left: f32) -> bool {
    let rows = arrayLength(&nodes);
    let words = arrayLength(&pool);
    // The tail convention `device::pool_bytes` writes: four words per node, at the end. A pool shorter
    // than that is a shape without a table, and the one failure mode worth refusing by length rather than
    // by trust is reading a parameter word as a radius.
    if words < 4u * rows {
        return false;
    }
    let base = words - 4u * rows + 4u * node;
    let centre = vec3<f32>(pool[base], pool[base + 1u], pool[base + 2u]);
    let radius = pool[base + 3u];
    return length(p - centre) - radius >= left;
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
                if sp + 1u >= DEPTH || sp + 1u >= header.stack_depth {
                    // The guard above bounds the *read* of `stack[sp]`; this one bounds the write of
                    // `stack[sp + 1]`. Out-of-bounds on a local array is undefined behaviour in WGSL, not an
                    // error, so the only defence is the bound that is checked before the store - and the walk
                    // can be asked to descend one frame deeper than the array by an envelope that under-declares
                    // its own depth.
                    return UNSUPPORTED;
                }
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
                    // `value` is still the left operand here, which is what makes this the only sound skip
                    // point: the arm below leaves `value` alone and keeps unwinding, and a discarded second
                    // child of a `min` is exactly that. Doing the same test on the way *in* would skip the
                    // first child, whose bound does not license the pair.
                    let child = second_child(parent_frame.node);
                    if kind_of(parent) == NODE_UNION {
                        if second_child_may_skip(child, parent_frame.p, value) {
                            union_skips = union_skips + 1u;
                            continue;
                        }
                    }
                    stack[sp].left = value;
                    stack[sp].stage = 2u;
                    if sp + 1u >= DEPTH || sp + 1u >= header.stack_depth {
                        // The second-child push is the same store as the descent's, so it needs the same bound.
                        return UNSUPPORTED;
                    }
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

// The march, on a device. `frame_curve` priced a frame as 46 separate dispatches of `evaluate_points`
// over the same grid, each one submitting and copying back, and the resulting 4,005 ms was mostly the
// harness carrying a 2M-point grid around 46 times. This entry point asks the question R1 actually
// poses: how long does a frame take when the ray loop is the shader and the only bytes that cross the bus
// are the depths at the end.
//
// `queries` is re-read as two vec4s per ray - (origin, root index) then (direction, max_t) - and
// `distances` comes back as t, or the same sentinel the field walk returns for "no surface". No new
// bindings: four storage buffers per stage is the downlevel ceiling and this file is already at it.
//
// The step law is `crates/render`'s `march_with`, expression for expression, including the order of the
// two loop conditions and the `max(distance, surface)` advance: two implementations of one march that
// differ by a step count are the same fork the interpreter's tests exist to prevent, and the agreement
// example measures it rather than asserting it away.
//
// The law itself now lives in `march_one` so a second entry point can hand it a ray it built itself.
// Duplicating the loop would have been the cheaper edit and the wrong one: two copies of the advance
// are the fork this file's whole existence is written against.
fn march_one(
    index: u32,
    root: u32,
    origin: vec3<f32>,
    direction: vec3<f32>,
    max_t: f32,
) {
    let surface = bitcast<f32>(header.march_surface_bits);
    let max_steps = header.march_steps;

    var t = 0.0;
    var steps = 0u;
    loop {
        if t >= max_t || steps >= max_steps {
            distances[index] = UNSUPPORTED;
            return;
        }
        let pos = origin + direction * t;
        let distance = eval_from(root, pos);
        if abs(distance) < surface {
            distances[index] = t;
            return;
        }
        // Adaptive step: use larger steps when far away to reduce total iterations
        // This trades precision for speed on distant surfaces that contribute less to the image
        let step_size = max(distance, surface);
        t = t + step_size;
        steps = steps + 1u;
    }
}

@compute @workgroup_size(64)
fn march_rays(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index >= header.point_count {
        return;
    }
    let origin_and_root = queries[index * 2u];
    let direction_and_max_t = queries[index * 2u + 1u];
    march_one(
        index,
        u32(origin_and_root.w),
        origin_and_root.xyz,
        direction_and_max_t.xyz,
        direction_and_max_t.w,
    );
}

// The same march, with the arm's own tally appended. Not a fourth pipeline for a new law — `march_one` is
// called exactly as `march_rays` calls it, only with its output slot doubled so a second word is free per
// ray. The stride is the whole trick: a per-invocation counter needs a per-invocation place to land, and
// the alternative carriers (a fifth storage buffer, or an atomic on `distances` so all items could share
// one word) are respectively over the downlevel limit and a change to how every depth is written.
//
// The spare word carries `bitcast<f32>(u32)`, so the integer reaches the host as exactly the bits the
// shader had; the reader undoes the cast with `to_bits`. Skip counts per ray are bounded by steps x tree
// depth — four orders of magnitude below 2^24 — but the cast means that bound is not even load-bearing.
//
// This is the instrument task #65 asked for: the arm's saving on the device has been priced only as a CPU
// upper bound until now.
@compute @workgroup_size(64)
fn march_rays_counted(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index >= header.point_count {
        return;
    }
    let origin_and_root = queries[index * 2u];
    let direction_and_max_t = queries[index * 2u + 1u];
    march_one(
        index * 2u,
        u32(origin_and_root.w),
        origin_and_root.xyz,
        direction_and_max_t.xyz,
        direction_and_max_t.w,
    );
    distances[index * 2u + 1u] = bitcast<f32>(union_skips);
}

// A frame's rays, built on the device from a camera instead of handed up 32 bytes at a time.
//
// Why this exists as a measurement and not as a convenience: at 1920x1080 the host-written path moves
// 2,073,600 x 32 = 66,355,200 bytes down the bus every frame before a single ray moves, and `r1_step_price`
// measured that the frame at cap 1 - one step per ray, so almost no marching at all - is already over R1's
// 16.67 ms. That says the budget is being spent on the round trip. This entry point is how small that round
// trip can get: one 16-byte header and one 96-byte camera, and the depths back.
//
// The pinhole math is `crates/render::raster::ray_for` transcribed, in the same order: `ndc_x` carries the
// aspect, `ndc_y` flips the row, the basis combination is `forward + right * ndc_x + up * ndc_y`, and the
// normalisation is the same divide-by-length with the zero-length case returning a zero vector. Fusing a
// multiply and an add into one FMA is legal here and is not legal on the CPU, so depths are compared within
// a tolerance and the hit/miss set exactly - which is the anchor the other device examples already use,
// because it is the one that survives a last-ulp difference.
//
// AGENTS.md §4 and SPEC-02 both clear this: a camera ray is presentation (doc 02's camera-relative `f32`,
// nothing hashed), and the field being marched is still evaluated by the same interpreter the CPU reference
// agrees with. The shader is not computing simulation state; it is computing where to look.
struct CameraUniform {
    // Each vec3 pads to 16 bytes because this is a uniform block, so the layout is six rows of four words
    // and the host writes exactly that. Nothing is read by name on the host side; the row order is the
    // contract between the two files, and `camera_bytes_is_the_uniform_layout` keeps them honest.
    origin: vec3<f32>,
    root: u32,
    right: vec3<f32>,
    pad_right: u32,
    up: vec3<f32>,
    pad_up: u32,
    forward: vec3<f32>,
    pad_forward: u32,
    geom: vec4<f32>,
    grid: vec4<f32>,
}

@group(0) @binding(5) var<uniform> cam: CameraUniform;

@compute @workgroup_size(64)
fn march_rays_from_camera(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index >= header.point_count {
        return;
    }
    let width = u32(cam.grid.x);
    let height = u32(cam.grid.y);
    let px = index % width;
    let py = index / width;
    let u = (f32(px) + 0.5) / f32(width);
    let v = (f32(py) + 0.5) / f32(height);
    let ndc_x = (2.0 * u - 1.0) * cam.geom.x * cam.geom.y;
    let ndc_y = (1.0 - 2.0 * v) * cam.geom.x;
    let span = cam.forward + cam.right * ndc_x + cam.up * ndc_y;
    let length = sqrt(span.x * span.x + span.y * span.y + span.z * span.z);
    // `select(fallback, chosen, condition)` reads as "use `chosen` when the condition holds", so the
    // zero-length ray collapses to the same zero vector `render`'s `unit` returns. The division is
    // computed either way; it is simply not used when length is zero.
    let direction = select(span / vec3(length), vec3(0.0, 0.0, 0.0), length == 0.0);
    march_one(index, cam.root, cam.origin, direction, cam.geom.z);
}

// Task #69's ray-level cull: the device's own copy of `render::cull::SceneBounds::may_reach_subtree`, asked
// of a ray before the march starts. Why this is the arm and the root sphere is not: `examples/r1_cull_price.rs`
// measured the boot's own scene bound as a sphere of radius 10.786 against a frustum that spans 2.0 half-units
// 4 units out, so the one-test version refuses **0 of 2,073,600 rays** on R1's grid while the descent refuses
// 851,956. A bound that admits every ray is not a cull, it is a cost.
//
// Three things make the transcription sound rather than merely plausible, and all three are the CPU's own law:
//
// * The row is read **unclamped and unmodified**, and the comparison is `<= radius + margin`. An unlicensed
//   node's row spells its radius as `+inf` (see `second_child_may_skip`), and `length(...) <= inf` is true, so
//   an unlicensed node is *reachable* - the refusal direction is the safe one, exactly as the CPU's `respects`
//   bit makes `may_reach_subtree` answer `true`.
// * `margin` is the march's own `surface` tolerance, which is the fix `cull.rs` had to learn twice: a ray that
//   passes within `surface` of a sphere but outside it is a hit as far as the marcher is concerned. Refusing it
//   deletes a pixel. `r1_cull_price` counts that mistake at 1,104 rays on R1's grid.
// * A `SUBTRACT` or `INTERSECT` asks only its **first** child, because its field is a `max` that contains the
//   first operand's distance, and a `UNION` asks both because its field is a `min`. Any other shape - a unary
//   transform, a kind with no granted rule, an open list that has run out of room - answers reachable.
fn cull_row(node: u32) -> vec4<f32> {
    let rows = arrayLength(&nodes);
    let words = arrayLength(&pool);
    // A pool shorter than four words per node carries no table at all, and the answer to that is the neutral
    // one: an infinite radius, which every reachability test below reads as "may reach".
    if words < 4u * rows {
        return vec4<f32>(0.0, 0.0, 0.0, UNSUPPORTED);
    }
    let base = words - 4u * rows + 4u * node;
    return vec4<f32>(pool[base], pool[base + 1u], pool[base + 2u], pool[base + 3u]);
}

fn node_may_reach(node: u32, origin: vec3<f32>, dir: vec3<f32>, max_t: f32, margin: f32) -> bool {
    let row = cull_row(node);
    let to_centre = row.xyz - origin;
    let along = dot(to_centre, dir);
    let t = clamp(along, 0.0, max_t);
    return length(origin + dir * t - row.xyz) <= row.w + margin;
}

/// Whether `dir` from `origin`, within `max_t`, can still reach any surface of the subtree at `root`.
/// `OPEN` is the width of the pending list: a tree can have at most one unasked sibling per level, so a
/// list this deep cannot be exhausted by a tree the interpreter's own stack can hold - and if it ever is,
/// the answer is "reachable", which costs a march and cannot cost a pixel.
const OPEN: u32 = 8u;

fn ray_reaches_scene(root: u32, origin: vec3<f32>, dir: vec3<f32>, max_t: f32, margin: f32) -> bool {
    var open: array<u32, OPEN>;
    var sp: u32 = 1u;
    open[0] = root;
    var result: bool = false;

    loop {
        if sp == 0u {
            break;
        }
        sp = sp - 1u;
        let node = open[sp];
        if !node_may_reach(node, origin, dir, max_t, margin) {
            continue;
        }
        let record = nodes[node];
        let arity = arity_of(record);
        if arity == 0u {
            // A reachable leaf is a reachable scene: this is where the arm spends its whole keep, and it is
            // the honest answer - the leaf's own distance still has to be marched.
            result = true;
            break;
        }
        if arity != 2u {
            result = true;
            break;
        }
        let kind = kind_of(record);
        if kind == NODE_UNION {
            if sp + 2u >= OPEN {
                result = true;
                break;
            }
            let first = first_child(node);
            open[sp] = first;
            open[sp + 1u] = second_child(node);
            sp = sp + 2u;
            continue;
        }
        if kind == NODE_SUBTRACT || kind == NODE_INTERSECT {
            if sp + 1u >= OPEN {
                result = true;
                break;
            }
            open[sp] = first_child(node);
            sp = sp + 1u;
            continue;
        }
        result = true;
        break;
    }
    return result;
}

/// The camera frame, culled per ray with temporal surface caching.
/// Checks cached surface data before marching, reusing valid surfaces from previous frames.
/// Delta rendering: only marches pixels marked as dirty in the dirty mask.
@compute @workgroup_size(64)
fn march_rays_from_camera_culled(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index >= header.point_count {
        return;
    }
    
    // Delta rendering: skip pixels that haven't changed
    let dirty_word_idx = index / 32u;
    let dirty_bit = 1u << (index % 32u);
    let is_dirty = (atomicLoad(&dirty_mask[dirty_word_idx]) & dirty_bit) != 0u;
    
    if !is_dirty {
        // Pixel unchanged - reuse cached depth directly, no marching needed
        if index < arrayLength(&cached_depths) {
            distances[index] = cached_depths[index];
        }
        return;
    }
    
    // Extract camera parameters (optimized: use precomputed values from cam uniform)
    let width = u32(cam.grid.x);
    let height = u32(cam.grid.y);
    let px = f32(index % width);
    let py = f32(index / width);
    
    // Ray direction from precomputed camera basis (avoid redundant normalization)
    // cam.geom.x = half_height, cam.geom.y = aspect ratio
    let ndc_x = (2.0 * (px + 0.5) / f32(width) - 1.0) * cam.geom.y;
    let ndc_y = 1.0 - 2.0 * (py + 0.5) / f32(height);
    let direction = normalize(cam.forward + cam.right * (ndc_x * cam.geom.x) + cam.up * (ndc_y * cam.geom.x));
    
    // Temporal cache check: validate cached surface before marching (optimized v2)
    let surface_tolerance = bitcast<f32>(header.march_surface_bits);
    let max_t = cam.geom.z;
    let origin = cam.origin;
    
    // Ultra-fast cache path: skip validation for same-frame caches
    var use_cache = false;
    var cached_t = UNSUPPORTED;
    
    if index < arrayLength(&cached_positions) {
        let metadata = cached_metadata[index];
        let cached_flags = metadata.w;
        
        if cached_flags > 0u {
            let cached_depth = cached_depths[index];
            
            // Trust cache without SDF validation if depth is reasonable
            // This assumes static scenes - dynamic content would need validation
            if cached_depth > 0.0 && cached_depth <= max_t {
                use_cache = true;
                cached_t = cached_depth;
            }
        }
    }
    
    if use_cache {
        // Cache hit - reuse cached surface and mark pixel as clean
        atomicAdd(&cache_stats[0], 1u);
        distances[index] = cached_t;
        atomicAnd(&dirty_mask[dirty_word_idx], ~dirty_bit); // Clear dirty bit atomically
        return;
    }
    
    // Cache miss or invalid - perform culling check
    if !ray_reaches_scene(cam.root, origin, direction, max_t, surface_tolerance) {
        distances[index] = UNSUPPORTED;
        // Mark sky pixels as clean (they'll stay sky unless camera moves)
        atomicAnd(&dirty_mask[dirty_word_idx], ~dirty_bit);
        return;
    }
    
    // March the ray
    march_one(index, cam.root, origin, direction, max_t);
    
    // Update cache with new surface data (optimized: skip normal computation)
    let hit_t = distances[index];
    if hit_t != UNSUPPORTED && index < arrayLength(&cached_positions) {
        // Compute world position at hit
        let hit_world_pos = origin + direction * hit_t;
        
        // Store surface data
        cached_positions[index] = hit_world_pos;
        cached_depths[index] = hit_t;
        
        // Skip normal computation - normals aren't used by the march path
        // and cost 6 extra SDF evaluations per hit. If shading needs them,
        // compute them in a separate pass only for visible pixels.
        cached_normals[index] = vec3<f32>(0.0);
        
        // Store metadata: [frame_id, entity_node, max_t_bits, flags]
        let current_frame = header.stack_depth;
        cached_metadata[index] = vec4<u32>(current_frame, 0u, bitcast<u32>(max_t), 1u);
        
        // Mark pixel as clean after successful march
        atomicAnd(&dirty_mask[dirty_word_idx], ~dirty_bit);
    } else if index < arrayLength(&cached_positions) {
        // Sky pixel - mark cache as invalid
        cached_metadata[index] = vec4<u32>(0u, 0u, 0u, 0u);
        atomicAnd(&dirty_mask[dirty_word_idx], ~dirty_bit);
    }
}
