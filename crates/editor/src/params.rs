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
    params_of_kind(crate::shape_path::kind(shape))
}

/// The same table read by the kind's name as the authored file spells it, for a caller that is holding a
/// name rather than a node — which is `add_part`, authoring a node that does not exist yet. An unknown
/// name reads as a combinator does (no parameters), so a typo costs a refusal at the field lookup rather
/// than inventing a range.
pub fn params_of_kind(kind: &str) -> &'static [Param] {
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

    match kind {
        "Sphere" => SPHERE,
        "RoundedBox" => ROUNDED_BOX,
        "Plane" => PLANE,
        "SmoothUnion" => SMOOTH_UNION,
        "Repeat" => REPEAT,
        "Mirror" => MIRROR,
        // A combinator carries no field of its own, and so does a name this file does not spell: the
        // caller's own lookup then refuses the field it asked for by naming the ones that exist.
        _ => NONE,
    }
}

/// The fields one kind takes, with their ranges, in the words a refusal answers with. One spelling for
/// the panel's slider, the pipe's `set_part` and the pipe's `add_part`, so a caller that reads a refusal
/// can paste back what it was offered.
pub fn field_text(kind: &str) -> String {
    let specs = params_of_kind(kind);
    if specs.is_empty() {
        return "none — a combinator's operands are reached by route".to_string();
    }
    specs
        .iter()
        .map(|spec| format!("{:?} ({:?})", spec.name, spec.range_text()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The kinds `add_part` can author from nothing: the leaves, which are the nodes that need no operands.
pub const AUTHORABLE: &[&str] = &["Sphere", "RoundedBox", "Plane"];

/// A node of an authorable kind carrying nothing yet, so every field the table publishes can be written
/// into it before it joins the tree. `None` for a kind [`AUTHORABLE`] does not list.
///
/// The zeros here are never what a saved file reads: [`write_param`] refuses a lane count that does not
/// match, so a field the table publishes and the caller did not supply stops the call rather than
/// surviving as a silent zero.
pub fn blank(kind: &str) -> Option<Shape> {
    match kind {
        "Sphere" => Some(Shape::Sphere {
            r: 0.0,
            centre: [0.0, 0.0, 0.0],
        }),
        "RoundedBox" => Some(Shape::RoundedBox {
            half: [0.0, 0.0, 0.0],
            r: 0.0,
        }),
        "Plane" => Some(Shape::Plane {
            normal: [0.0, 0.0, 0.0],
            offset: 0.0,
        }),
        _ => None,
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

/// The four noise spellings, in `entity::NoiseKind`'s declaration order.
///
/// A choice field needs one list rather than a match arm per surface: `set_material` refuses a name it
/// does not carry by printing this, the table publishes its limits through it, and the pipe reads back a
/// material's noise as the same four words.
pub const NOISE_KINDS: [&str; 4] = ["Voronoi", "Value", "Perlin", "None"];

/// One authored field of a material, its limit as `entity::Material` states it, and whether a shader
/// reads it at all.
///
/// The limits are not invented here — each is the field's own doc comment (`0.0–1.0 per channel`,
/// `0.0 = mirror, 1.0 = matte`, `>0.0 = glows`, `1.0 = air`), and a field whose comment states only one
/// end publishes only that end. `shaded` is the honest column: today exactly one field is true, because
/// doc 04 § Materials, lighting, sky leaves the shading model to OQ-10 and nothing has decided it.
#[derive(Debug, Clone, Copy)]
pub struct MaterialField {
    /// The field's name as the struct spells it: `base_color`, `roughness`, `noise`.
    pub name: &'static str,
    /// How many numbers the caller hands over: three for a colour, one for every other field.
    pub lanes: u8,
    pub lo: f32,
    /// `true` when `lo` itself is refused — a noise frequency of zero asks for a pattern that never varies.
    pub lo_open: bool,
    /// The ceiling the contract states. `None` means the comment says only "more is brighter / finer",
    /// so a refusal quotes the end that exists rather than a made-up one.
    pub hi: Option<f32>,
    /// A field admitted only as a whole number (`noise`, which is an index into [`NOISE_KINDS`]).
    pub whole: bool,
    /// The spelled-out domain, when the field is a choice rather than a length.
    pub choices: Option<&'static [&'static str]>,
    pub shaded: bool,
}

impl MaterialField {
    /// The limit in the words a refusal answers with, so the pipe cannot quote a range the struct's own
    /// comment does not state.
    pub fn limit_text(&self) -> String {
        if let Some(choices) = self.choices {
            return format!(
                "one of {}",
                choices
                    .iter()
                    .map(|kind| format!("{kind:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let lo = if self.lo_open {
            format!("> {}", self.lo)
        } else {
            format!(">= {}", self.lo)
        };
        match self.hi {
            Some(hi) => format!("{lo} and <= {hi}"),
            None => lo,
        }
    }
}

/// Every field a material carries, in `entity::Material`'s own declaration order — which is the order the
/// pipe prints one in, so a reader who has the struct in front of them and this table beside it can tell
/// whether a field went missing from the surface or from the type.
pub const MATERIAL_FIELDS: [MaterialField; 6] = [
    MaterialField {
        name: "base_color",
        lanes: 3,
        lo: 0.0,
        lo_open: false,
        hi: Some(1.0),
        whole: false,
        choices: None,
        shaded: true,
    },
    MaterialField {
        name: "roughness",
        lanes: 1,
        lo: 0.0,
        lo_open: false,
        hi: Some(1.0),
        whole: false,
        choices: None,
        shaded: false,
    },
    MaterialField {
        name: "emissive",
        lanes: 1,
        lo: 0.0,
        lo_open: false,
        hi: None,
        whole: false,
        choices: None,
        shaded: false,
    },
    MaterialField {
        name: "index_ior",
        lanes: 1,
        lo: 1.0,
        lo_open: false,
        hi: None,
        whole: false,
        choices: None,
        shaded: false,
    },
    MaterialField {
        name: "noise",
        lanes: 1,
        lo: 0.0,
        lo_open: false,
        hi: Some((NOISE_KINDS.len() - 1) as f32),
        whole: true,
        choices: Some(&NOISE_KINDS),
        shaded: false,
    },
    MaterialField {
        name: "noise_frequency",
        lanes: 1,
        lo: 0.0,
        lo_open: true,
        hi: None,
        whole: false,
        choices: None,
        shaded: false,
    },
];

/// The table read by field name, the way an author spells it.
pub fn material_field(name: &str) -> Option<&'static MaterialField> {
    MATERIAL_FIELDS.iter().find(|field| field.name == name)
}

/// The field names, split by whether a shader reads them — the two lists `editor_state` prints and the
/// sentence `set_material` appends, all from the one table so a claim and a refusal cannot disagree.
pub fn material_names_by_shading() -> (Vec<&'static str>, Vec<&'static str>) {
    let mut read: Vec<&'static str> = Vec::new();
    let mut unread: Vec<&'static str> = Vec::new();
    for field in MATERIAL_FIELDS {
        if field.shaded {
            read.push(field.name);
        } else {
            unread.push(field.name);
        }
    }
    (read, unread)
}

/// The values a material currently carries for one of its fields, exactly as many as the table says it
/// has. `None` means the field is not in the table — which is not a zero, so a caller can tell a field it
/// did not write from a field that happens to read zero.
pub fn read_material_field(material: &entity::Material, name: &str) -> Option<Vec<f32>> {
    let field = material_field(name)?;
    let lanes = match name {
        "base_color" => material.base_color.to_vec(),
        "roughness" => vec![material.roughness],
        "emissive" => vec![material.emissive],
        "index_ior" => vec![material.index_ior],
        "noise" => vec![match material.noise {
            entity::NoiseKind::Voronoi => 0.0,
            entity::NoiseKind::Value => 1.0,
            entity::NoiseKind::Perlin => 2.0,
            entity::NoiseKind::None => 3.0,
        }],
        "noise_frequency" => vec![material.noise_frequency],
        _ => return None,
    };
    // A table the reader and the writer disagree about would hand a caller a half-read field, so the
    // count is checked rather than assumed — the same drift `write_material_field` refuses on the way in.
    if lanes.len() != field.lanes as usize {
        return None;
    }
    Some(lanes)
}

/// Write one material field from the lanes the caller handed over. `None` if the field is not in the
/// table or the lane count does not match it — never a partial write, and never a silent skip.
pub fn write_material_field(
    material: &mut entity::Material,
    name: &str,
    lanes: &[f32],
) -> Option<()> {
    let field = material_field(name)?;
    if lanes.len() != field.lanes as usize {
        return None;
    }
    match name {
        "base_color" => material.base_color = [lanes[0], lanes[1], lanes[2]],
        "roughness" => material.roughness = lanes[0],
        "emissive" => material.emissive = lanes[0],
        "index_ior" => material.index_ior = lanes[0],
        "noise" => {
            material.noise = match lanes[0] as usize {
                0 => entity::NoiseKind::Voronoi,
                1 => entity::NoiseKind::Value,
                2 => entity::NoiseKind::Perlin,
                _ => entity::NoiseKind::None,
            }
        }
        "noise_frequency" => material.noise_frequency = lanes[0],
        _ => return None,
    }
    Some(())
}
