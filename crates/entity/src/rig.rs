//! Rigging without a skeleton: a pose rewrites primitive parameters.
//!
//! Doc 07 § Animation rejects bone rigs and skinning (ADR-011) and chooses a domain warp,
//! `SDF(T(p, t), t)`. The grammar still cannot express a *per-part* warp — that is OQ-55 in doc 13, whose
//! `Placement` is applied once per entity, host-side — so a walking animal looked impossible until the
//! contract moved. It is not.
//! A-07's sphere stores its centre as three parameters (the evaluator arm in `crates/render/src/lib.rs`
//! reads `params = [r, cx, cy, cz]`), so moving a sphere is three `f32` writes, and rotating a column of
//! spheres about a hip is that same rotation applied to each centre.
//!
//! The consequence that makes this worth having: **the record count, the tree shape and the march budget
//! are all unchanged by a pose.** Doc 04's cost law is `steps × nodes`, so a gait paid for in parameter
//! writes costs the frame nothing it was not already paying, while the alternative — one extra tree walk
//! per placed part — costs a full walk per part. That is a difference of arithmetic, not of taste.
//!
//! What this is *not*: it is not doc 07's warp. A primitive that stores no orientation — a `RoundedBox` —
//! cannot be turned by rewriting its parameters, so only parts authored as sphere columns (and the planes
//! that cut them) ride a joint here. Soft-tissue deformation, a breathing belly or a floppy ear, still needs
//! OQ-55's placement node or a shader-side warp. This file poses *rigid* parts and nothing else.
//!
//! §4's transcendental ban does not bite: a pose is presentation, the ledger hashes
//! `EntityState`'s 12-bit integer payload, and doc 07's own gait law is `sin(omega * t) * amp`.

use contracts::sdf::{NODE_PLANE, NODE_SPHERE};
use render::{Tree, Vec3};

/// A revolute joint and the records that ride on it, in the tree's own shape space.
///
/// `rides` holds record indices rather than a subtree range because a bone's records are not a contiguous
/// run of one kind: the pig's hoof is `Intersect(Union(toe, toe), Plane)`, so the joint carries three
/// spheres *and* the plane that cuts them, and a plane has no centre to select by position.
#[derive(Debug, Clone, PartialEq)]
pub struct Joint {
    /// The hinge point. Rotation is about the axis parallel to X through here — the axis a quadruped's
    /// limb turns on in the sagittal plane, which for this animal is the `y`/`z` plane.
    pub pivot: Vec3,
    /// Records this joint carries: sphere centres and plane cuts.
    pub rides: Vec<usize>,
}

/// One authored leg: a thigh hinged at the hip, a shank hinged at the knee.
#[derive(Debug, Clone, PartialEq)]
pub struct Leg {
    pub hip: Joint,
    pub knee: Joint,
}

impl Leg {
    /// Apply this pose, overwriting the riding records' parameters in place.
    ///
    /// The shank is carried by *both* hinges: the thigh's turn moves it before the knee's own turn bends it,
    /// and the knee's pivot travels with the hip. That composition is the whole of forward kinematics at two
    /// bones, and leaving either half out is a specific, silent wrong: hinging the shank about the **rest**
    /// knee pivots it in mid-air once the thigh has swung, and skipping the thigh's turn on the shank leaves
    /// the foot at its authored place whenever the knee happens not to flex — a limb that stretches out of
    /// its socket instead of swinging. Angles are radians, positive tips the limb toward `+z`; `(0.0, 0.0)`
    /// is the identity and writes nothing at all.
    pub fn pose(&self, tree: &mut Tree, hip_angle: f32, knee_angle: f32) {
        let knee_now = hinge(self.hip.pivot, hip_angle, self.knee.pivot);
        for &index in &self.hip.rides {
            write_record(tree, index, self.hip.pivot, hip_angle);
        }
        for &index in &self.knee.rides {
            write_record(tree, index, self.hip.pivot, hip_angle);
            write_record(tree, index, knee_now, knee_angle);
        }
    }

    /// Every record the pose touches — for a caller counting writes against the frame budget.
    pub fn rides(&self) -> impl Iterator<Item = usize> + '_ {
        self.hip
            .rides
            .iter()
            .copied()
            .chain(self.knee.rides.iter().copied())
    }

    /// The height of the surface this limb stands on, in shape space.
    ///
    /// Where the authored hoof carries a sole cut, the cut *is* the bottom: the `Intersect` removes
    /// everything below it, so the foot sphere's own lowest point is not surface at all and would sink the
    /// animal into the floor. Without a cut the widest sphere's bottom is the support, which is what a
    /// ball-footed creature would actually stand on.
    ///
    /// This exists because of what the first stride strip showed: a limb swung about a hip above its foot
    /// raises its support by `L(1 − cos θ)`, so a body that stays put floats at every non-zero phase. The
    /// caller keeps a foot on the ground by moving the *body* — doc 07's pose parameters, of which the
    /// vertical is one — and the distance to move it is this against the rest pose's value.
    pub fn support_y(&self, tree: &Tree) -> f32 {
        let centres: Vec<Vec3> = self
            .knee
            .rides
            .iter()
            .filter(|&&index| tree.nodes[index].kind() == NODE_SPHERE)
            .map(|&index| sphere_centre(tree, index))
            .collect();
        let cut = self
            .knee
            .rides
            .iter()
            .copied()
            .find(|&index| tree.nodes[index].kind() == NODE_PLANE);
        if let Some(plane) = cut {
            return centres
                .iter()
                .map(|&p| sole_height(tree, plane, p))
                .fold(f32::INFINITY, f32::min);
        }
        self.knee
            .rides
            .iter()
            .filter(|&&index| tree.nodes[index].kind() == NODE_SPHERE)
            .map(|&index| {
                let first = tree.nodes[index].params_first() as usize;
                sphere_centre(tree, index)[1] - tree.params[first]
            })
            .fold(f32::INFINITY, f32::min)
    }
}

/// The height of a plane's surface directly above `above`, in that point's `x`/`z` column.
///
/// Read straight off the evaluator's own law (`dot(n, p) - d`), so it stays true after [`write_record`]
/// turns the plane with its hoof — which is the point: a posed sole is a *tilted* sole, and the support
/// height under each toe is then genuinely different.
fn sole_height(tree: &Tree, plane: usize, above: Vec3) -> f32 {
    let first = tree.nodes[plane].params_first() as usize;
    let (nx, ny, nz, d) = (
        tree.params[first],
        tree.params[first + 1],
        tree.params[first + 2],
        tree.params[first + 3],
    );
    (d - nx * above[0] - nz * above[2]) / ny
}

/// Rotate a point about the axis parallel to X through `pivot`.
///
/// `x` is untouched by construction, so a column at one `x` stays a column however the limb swings. That
/// is also why `Mirror`'s twin swings in unison: the mirror reflects `x` only, and an X-parallel hinge
/// moves nothing in `x`, so the two sides cannot be given different phases from one authored leg. A diagonal
/// gait therefore needs either both sides authored (twice the nodes) or OQ-55's placement node — see doc 07.
fn hinge(pivot: Vec3, angle: f32, point: Vec3) -> Vec3 {
    if angle == 0.0 {
        return point;
    }
    let (s, c) = (angle.sin(), angle.cos());
    let dy = point[1] - pivot[1];
    let dz = point[2] - pivot[2];
    [
        point[0],
        pivot[1] + dy * c - dz * s,
        pivot[2] + dy * s + dz * c,
    ]
}

/// Rewrite one riding record for a hinge about `pivot`.
fn write_record(tree: &mut Tree, index: usize, pivot: Vec3, angle: f32) {
    if angle == 0.0 {
        return;
    }
    let first = tree.nodes[index].params_first() as usize;
    if tree.nodes[index].kind() == NODE_SPHERE {
        let centre = hinge(
            pivot,
            angle,
            [
                tree.params[first + 1],
                tree.params[first + 2],
                tree.params[first + 3],
            ],
        );
        tree.params[first + 1] = centre[0];
        tree.params[first + 2] = centre[1];
        tree.params[first + 3] = centre[2];
        return;
    }

    // A plane reads `dot(n, p) - d`, so carrying it rigidly means rotating its normal and re-solving `d`
    // from the turned support point. Skip that and the sole cut stays at the rest height while the hoof
    // lifts, so a raised foot comes back sliced by an invisible cutter.
    let d = tree.params[first + 3];
    let normal = [
        tree.params[first],
        tree.params[first + 1],
        tree.params[first + 2],
    ];
    let support = [normal[0] * d, normal[1] * d, normal[2] * d];
    let turned_normal = hinge([0.0; 3], angle, normal);
    let turned_support = hinge(pivot, angle, support);
    tree.params[first] = turned_normal[0];
    tree.params[first + 1] = turned_normal[1];
    tree.params[first + 2] = turned_normal[2];
    tree.params[first + 3] = turned_normal[0] * turned_support[0]
        + turned_normal[1] * turned_support[1]
        + turned_normal[2] * turned_support[2];
}

/// The signed distance a plane record reports for a point — the same expression the evaluator arm uses,
/// so an invariant stated here is stated about the field, not about a parallel definition of it.
pub fn plane_distance(tree: &Tree, index: usize, point: Vec3) -> f32 {
    let first = tree.nodes[index].params_first() as usize;
    tree.params[first] * point[0]
        + tree.params[first + 1] * point[1]
        + tree.params[first + 2] * point[2]
        - tree.params[first + 3]
}

/// A sphere record's centre.
pub fn sphere_centre(tree: &Tree, index: usize) -> Vec3 {
    let first = tree.nodes[index].params_first() as usize;
    [
        tree.params[first + 1],
        tree.params[first + 2],
        tree.params[first + 3],
    ]
}

/// Where one authored leg's joints sit, in shape space.
///
/// The pivots are authoring knowledge, not geometry: nothing in the tree says where a hip is, and inferring
/// one from the topmost sphere puts the hinge at the surface of the flesh it is supposed to be hidden by.
/// `z` is the authored limb column, matched within `tolerance` because the compiled pool holds the `f32` the
/// RON wrote, and a blend or a mirror can land a hair off it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegSpec {
    pub z: f32,
    pub tolerance: f32,
    pub hip_y: f32,
    pub knee_y: f32,
}

/// Find one leg in a compiled tree, or refuse.
///
/// Call this on the **rest** pose and keep the result. Selection is geometric, and a swung limb's column no
/// longer sits at the authored `z`, so selecting from a posed tree would find a leg that has already moved —
/// or, past the tolerance, find nothing. The record indices a rest pose yields stay valid for every pose
/// after it, because a pose rewrites parameters and no other part of the record.
///
/// Selection is geometric because a hand-written list of record indices rots the moment the authoring file
/// gains a sphere — and rots *silently*, since a wrong index is still a valid record. The spheres in the
/// band define the limb's own subtree through the contract's `subtree_len`; the plane that cuts the hoof is
/// then whatever plane record lies inside that subtree, which is how a sole travels with its foot without the
/// file naming the plane twice.
///
/// Refuses: no sphere in the band; a band with nothing above or nothing below `knee_y`; a hip below the
/// topmost sphere of the bone it turns (that shears the column through its own flesh instead of swinging
/// it); a band whose spheres are not inside one subtree; a non-unit plane normal, which the rigid carry in
/// [`write_record`] does not preserve.
pub fn select_leg(tree: &Tree, spec: &LegSpec) -> Option<Leg> {
    let band: Vec<usize> = (0..tree.nodes.len())
        .filter(|&index| sphere_in_band(tree, index, spec))
        .collect();
    if band.is_empty() {
        return None;
    }

    let thigh: Vec<usize> = band
        .iter()
        .copied()
        .filter(|&index| sphere_centre(tree, index)[1] >= spec.knee_y)
        .collect();
    let shank: Vec<usize> = band
        .iter()
        .copied()
        .filter(|&index| sphere_centre(tree, index)[1] < spec.knee_y)
        .collect();
    if thigh.is_empty() || shank.is_empty() {
        return None;
    }
    let top_of_bone = thigh
        .iter()
        .map(|&index| sphere_centre(tree, index)[1])
        .fold(f32::NEG_INFINITY, f32::max);
    if spec.hip_y < top_of_bone {
        return None;
    }

    let (span_start, span_end) = minimal_span(tree, &band)?;
    let mut rides = shank;
    let cuts: Vec<usize> = (span_start..span_end)
        .filter(|&index| tree.nodes[index].kind() == NODE_PLANE)
        .collect();
    if !cuts.iter().all(|&index| plane_normal_is_unit(tree, index)) {
        return None;
    }
    rides.extend(cuts);

    Some(Leg {
        hip: Joint {
            pivot: [0.0, spec.hip_y, spec.z],
            rides: thigh,
        },
        knee: Joint {
            pivot: [0.0, spec.knee_y, spec.z],
            rides,
        },
    })
}

fn sphere_in_band(tree: &Tree, index: usize, spec: &LegSpec) -> bool {
    let record = tree.nodes[index];
    if record.kind() != NODE_SPHERE || record.params_len() < 4 {
        return false;
    }
    (sphere_centre(tree, index)[2] - spec.z).abs() <= spec.tolerance
}

fn plane_normal_is_unit(tree: &Tree, index: usize) -> bool {
    let first = tree.nodes[index].params_first() as usize;
    let squared = tree.params[first] * tree.params[first]
        + tree.params[first + 1] * tree.params[first + 1]
        + tree.params[first + 2] * tree.params[first + 2];
    (squared - 1.0).abs() < 1e-6
}

/// The smallest `[start, start + subtree_len)` covering every index in `band`.
///
/// In a pre-order buffer that span is the limb's lowest common ancestor, and it is unique: a subtree span
/// covering several leaves has to start at or before the first and end at or after the last, so the shortest
/// such run is the innermost one. Refusing to guess is the point — if the band came from two limbs, the
/// plane search would attach one leg's sole to the other's joint, and the picture would be wrong in a way
/// no validator describes.
fn minimal_span(tree: &Tree, band: &[usize]) -> Option<(usize, usize)> {
    let (low, high) = band
        .iter()
        .fold((usize::MAX, 0usize), |acc, &i| (acc.0.min(i), acc.1.max(i)));
    if low == high {
        return Some((low, low + 1));
    }
    (0..=low)
        .filter(|&start| tree.nodes[start].arity() > 0)
        .map(|start| (start, start + tree.nodes[start].subtree_len() as usize))
        .filter(|&(start, end)| start <= low && end > high)
        .min_by_key(|&(start, end)| end - start)
}

/// One stride's worth of angles, as a pure function of phase.
///
/// Two parameters and no state, because a gait that has to remember its previous frame cannot be replayed,
/// and replay is the reason an animal can be drawn at `t = 3.7` by a peer that never saw `t = 3.6`. The knee
/// is `max(0, sin)` rather than a bare sine: a limb that hyperextends backward reads as broken at exactly the
/// phase where a real limb is loaded, and doc 07's own warning is to keep warp amplitudes modest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gait {
    /// Cycles per `2 pi` of phase. `1.0` is one stride per `2 pi`.
    pub omega: f32,
    /// Hip amplitude, radians.
    pub swing: f32,
    /// Knee amplitude through the swing half, radians.
    pub flex: f32,
}

impl Gait {
    /// `(hip, knee)` for a limb at `phase` radians of its own cycle.
    pub fn angles(&self, phase: f32) -> (f32, f32) {
        let s = (phase * self.omega).sin();
        (self.swing * s, -self.flex * s.max(0.0))
    }
}
