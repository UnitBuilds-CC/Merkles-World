//! The parameters a node carries, the way the inspector's sliders offer them.
//!
//! One table, read by both the egui panel and the MCP pipe: the panel drags a value inside a range and
//! the pipe refuses a value outside it, and if those two ranges were written down twice they would drift
//! into an edit an agent can make that the window cannot, which is the exact disagreement doc 16's gate
//! E7 exists to catch.

use assetc::Shape;

/// One authored field of one node, named the way the `.ron` file and `assetc`'s refusals name it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Param {
    /// The field's name in the authored file: `r`, `centre`, `k`, `axis`.
    pub name: &'static str,
    /// The text the panel puts beside its slider, so a reader can find the same control by eye.
    pub label: &'static str,
    pub lanes: u8,
    pub lo: f32,
    pub hi: f32,
    /// A field the file carries as an integer rather than a length (`Mirror::axis`), so the pipe can
    /// refuse `axis: 1.5` instead of quietly rounding it to a different animal.
    pub whole: bool,
}

impl Param {
    /// The range as a refusal should quote it — one spelling, so the panel's slider and the pipe's error
    /// text cannot drift into describing different limits.
    pub fn range_text(&self) -> String {
        format!("{}..={}", self.lo, self.hi)
    }
}

/// The node's own parameters, in the order the panel lists them. A combinator carries none: its operands
/// are reached by route, which is the whole point of addressing nodes by part.
pub fn params_of(shape: &Shape) -> &'static [Param] {
    const SPHERE: &[Param] = &[
        Param {
            name: "r",
            label: "radius",
            lanes: 1,
            lo: 0.01,
            hi: 5.0,
            whole: false,
        },
        Param {
            name: "centre",
            label: "centre",
            lanes: 3,
            lo: -5.0,
            hi: 5.0,
            whole: false,
        },
    ];
    const ROUNDED_BOX: &[Param] = &[
        Param {
            name: "half",
            label: "half extents",
            lanes: 3,
            lo: 0.01,
            hi: 5.0,
            whole: false,
        },
        Param {
            name: "r",
            label: "corner radius",
            lanes: 1,
            lo: 0.0,
            hi: 2.0,
            whole: false,
        },
    ];
    const PLANE: &[Param] = &[
        Param {
            name: "normal",
            label: "normal",
            lanes: 3,
            lo: -1.0,
            hi: 1.0,
            whole: false,
        },
        Param {
            name: "offset",
            label: "offset",
            lanes: 1,
            lo: -10.0,
            hi: 10.0,
            whole: false,
        },
    ];
    const SMOOTH_UNION: &[Param] = &[Param {
        name: "k",
        label: "blend k",
        lanes: 1,
        lo: 0.0,
        hi: 1.0,
        whole: false,
    }];
    const REPEAT: &[Param] = &[Param {
        name: "period",
        label: "period",
        lanes: 1,
        lo: 0.05,
        hi: 10.0,
        whole: false,
    }];
    const MIRROR: &[Param] = &[Param {
        name: "axis",
        label: "axis (x y z)",
        lanes: 1,
        lo: 0.0,
        hi: 2.0,
        whole: true,
    }];
    const NONE: &[Param] = &[];

    match shape {
        Shape::Sphere { .. } => SPHERE,
        Shape::RoundedBox { .. } => ROUNDED_BOX,
        Shape::Plane { .. } => PLANE,
        Shape::SmoothUnion { .. } => SMOOTH_UNION,
        Shape::Repeat { .. } => REPEAT,
        Shape::Mirror { .. } => MIRROR,
        Shape::Union { .. } | Shape::Subtract { .. } | Shape::Intersect { .. } => NONE,
    }
}

/// The lanes a node currently carries for one of its parameters, right-padded with zeros so a caller can
/// hand the same `[f32; 3]` to a scalar or a vector field. `None` is not a zero: it means this node does
/// not have that field.
pub fn read_param(shape: &Shape, name: &str) -> Option<[f32; 3]> {
    let lanes = params_of(shape)
        .iter()
        .find(|param| param.name == name)?
        .lanes;
    let mut out = [0.0; 3];
    match (name, shape) {
        ("r", Shape::Sphere { r, .. }) => out[0] = *r,
        ("centre", Shape::Sphere { centre, .. }) => out[..3].copy_from_slice(centre),
        ("half", Shape::RoundedBox { half, .. }) => out[..3].copy_from_slice(half),
        ("r", Shape::RoundedBox { r, .. }) => out[0] = *r,
        ("normal", Shape::Plane { normal, .. }) => out[..3].copy_from_slice(normal),
        ("offset", Shape::Plane { offset, .. }) => out[0] = *offset,
        ("k", Shape::SmoothUnion { k, .. }) => out[0] = *k,
        ("period", Shape::Repeat { period, .. }) => out[0] = *period,
        ("axis", Shape::Mirror { axis, .. }) => out[0] = *axis as f32,
        _ => return None,
    }
    // A vector field has three lanes and a scalar has one; the padding must not become data a writer
    // could read back as authored.
    for (index, lane) in out.iter_mut().enumerate() {
        if index >= lanes as usize {
            *lane = 0.0;
        }
    }
    Some(out)
}

/// Write one named parameter of this node from the lanes the caller handed over. `None` if the node does
/// not carry that field, or the lane count does not match it — the pipe's refusal, not a silent skip, and
/// never a partial write: every lane is checked before the first one lands.
pub fn write_param(shape: &mut Shape, name: &str, lanes: &[f32]) -> Option<()> {
    let want = params_of(shape)
        .iter()
        .find(|param| param.name == name)?
        .lanes as usize;
    if lanes.len() != want {
        return None;
    }
    let three = |out: &mut [f32; 3]| {
        for (lane, value) in out.iter_mut().zip(lanes) {
            *lane = *value;
        }
    };
    match (name, shape) {
        ("r", Shape::Sphere { r, .. }) => *r = lanes[0],
        ("centre", Shape::Sphere { centre, .. }) => three(centre),
        ("half", Shape::RoundedBox { half, .. }) => three(half),
        ("r", Shape::RoundedBox { r, .. }) => *r = lanes[0],
        ("normal", Shape::Plane { normal, .. }) => three(normal),
        ("offset", Shape::Plane { offset, .. }) => *offset = lanes[0],
        ("k", Shape::SmoothUnion { k, .. }) => *k = lanes[0],
        ("period", Shape::Repeat { period, .. }) => *period = lanes[0],
        ("axis", Shape::Mirror { axis, .. }) => *axis = lanes[0] as u8,
        _ => return None,
    }
    Some(())
}
