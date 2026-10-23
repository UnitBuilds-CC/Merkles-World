//! Entity editor: visual SDF authoring tool with an egui UI.
//!
//! This is a **tool** (AGENTS.md §3), not part of the runtime hot path. It enables:
//! - A viewport that marches the entity with `crates/render`'s CPU reference — the same evaluator the
//!   WGSL interpreter has to agree with. Not the GPU: eframe owns the window's device, `gpu::Interpreter`
//!   owns its own, and no temporal cache is wired into this path yet.
//! - Editing the authored transform: `position`, `scale` and `rotation` by slider, all three under a
//!   heading that is open by default. `rotation` is shown as an axis and a turn in degrees and written as
//!   the unit quaternion the viewport requires; the four numbers the file will carry are printed under the
//!   sliders, and a quaternion that is not unit is named there rather than quietly refused by the renderer.
//!   Which representation the *file* uses is still OQ-63's — the panel composes the field that exists, it
//!   does not settle it. See docs/16 § Visual authoring.
//! - Editing the parameters of any node in the tree: the panel lists the parts by their authored route
//!   (`root.b.kept`) with the node's kind beside it, and the sliders below the list write to the selected
//!   one. The field names in a route are the ones `assetc`'s refusals use, so an error text and a row
//!   point at the same part. The fields themselves, and the range each one may take, are one table
//!   (`params`) that the panel draws from and the MCP pipe validates against — two copies of a range would
//!   drift into an edit an agent can make that the window cannot.
//! - Material slot assignment and a swatch read out of the same registry the viewport shades with
//! - RON load and save, with dirty tracking
//!
//! Usage: `cargo run -p editor --example entity_editor`, or `--example render_png` to write the same
//! frames out when there is no one to look at a window. An agent driving the editor without a window uses
//! `cargo run -p editor --bin editor_mcp`, which serves the same `EditorState` over MCP on stdin/stdout —
//! see `mcp` and docs/16 § Driving the editor over MCP.

pub mod export;
pub mod gpu_renderer;
pub mod inspector;
pub mod material_panel;
pub mod mcp;
pub mod params;
pub mod shape_path;
pub mod viewport;

use core::hash::{Hash, Hasher};

use assetc::{EntityDef, Shape};
use entity::MaterialRegistry;
use shape_path::Step;

/// One edit's resting point: everything an author can change and would expect back.
///
/// The material registry is in here because a slot's `base_color` is a pixel; an undo that restored the
/// entity but left a recoloured slot would put back a different animal than the one before the call.
/// The registry is process memory rather than part of the entity file, which is why a load starts a new
/// document instead of restoring a snapshot — see [`EditorState::load_ron`].
#[derive(Clone, PartialEq)]
pub struct Snapshot {
    entity_def: EntityDef,
    materials: MaterialRegistry,
    selected: Vec<Step>,
    dirty: bool,
}

/// Editor state: tracks the currently loaded entity and dirty flag
pub struct EditorState {
    /// Current entity definition being edited
    pub entity_def: EntityDef,
    /// The materials a `material_id` may point at. One registry, read by both the panel's swatches
    /// and the viewport's shading, so the two cannot disagree about what slot 0 looks like.
    pub materials: MaterialRegistry,
    /// Which node the shape sliders write to. Empty is the root, and a path that no longer resolves —
    /// after a load, or after a rebuild of the tree — is reported as gone rather than silently
    /// retargeted at the root, because editing the parent instead of the part is exactly the change
    /// the file would not describe.
    pub selected: Vec<Step>,
    /// Whether parameters have been modified since last save
    pub dirty: bool,
    /// Path to loaded file (if any)
    pub file_path: Option<String>,
    /// Edits already made, newest last. The oldest is dropped past [`EditorState::HISTORY_DEPTH`].
    undo_stack: Vec<Snapshot>,
    /// Edits taken back, offered to `redo` until a new edit clears the branch.
    redo_stack: Vec<Snapshot>,
}

impl EditorState {
    /// Create new editor state with a default sphere entity
    pub fn new() -> Self {
        Self {
            entity_def: EntityDef {
                shape: Shape::Sphere {
                    r: 1.0,
                    centre: [0.0, 0.0, 0.0],
                },
                position: [0.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: 1.0,
                material_id: 0,
                flags: 0x03, // ALIVE | ACTIVE
                quantity: 0,
            },
            materials: MaterialRegistry::with_defaults(),
            selected: Vec::new(),
            dirty: false,
            file_path: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// How many edits the history remembers.
    ///
    /// Bounded because this is a tool process an agent can drive for thousands of calls: a snapshot
    /// stack that only grows is memory nothing frees, and the oldest steps of a long session are the
    /// ones an author is least likely to want back.
    pub const HISTORY_DEPTH: usize = 32;

    /// The values an edit would change, captured before the edit is attempted.
    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            entity_def: self.entity_def.clone(),
            materials: self.materials.clone(),
            selected: self.selected.clone(),
            dirty: self.dirty,
        }
    }

    /// Keep `before` as a step if the call actually changed something; returns whether it did.
    ///
    /// A call that wrote the value it already had is not a step: undo would spend a gesture putting
    /// back an identical tree and the next undo would look like it failed. A real edit also clears
    /// the redo branch, because a history with two futures is not one an author can reason about.
    pub(crate) fn remember(&mut self, before: Snapshot) -> bool {
        if before == self.snapshot() {
            return false;
        }
        self.undo_stack.push(before);
        if self.undo_stack.len() > Self::HISTORY_DEPTH {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        true
    }

    /// Put the entity back one edit, offering what it replaces to `redo`.
    pub(crate) fn undo(&mut self) -> Result<(), String> {
        let Some(before) = self.undo_stack.pop() else {
            return Err(
                "there is nothing to undo: no call has changed the entity since it was loaded"
                    .to_string(),
            );
        };
        let now = self.snapshot();
        self.restore(before);
        self.redo_stack.push(now);
        Ok(())
    }

    /// Re-apply the edit `undo` took back, as long as no new edit has landed since.
    pub(crate) fn redo(&mut self) -> Result<(), String> {
        let Some(after) = self.redo_stack.pop() else {
            return Err(
                "there is nothing to redo: no edit has been undone since the last change"
                    .to_string(),
            );
        };
        let now = self.snapshot();
        self.restore(after);
        self.undo_stack.push(now);
        Ok(())
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.entity_def = snapshot.entity_def;
        self.materials = snapshot.materials;
        self.selected = snapshot.selected;
        self.dirty = snapshot.dirty;
    }

    /// How many edits `undo` can still take back, and how many `redo` can re-apply.
    pub(crate) fn history_depth(&self) -> (usize, usize) {
        (self.undo_stack.len(), self.redo_stack.len())
    }

    /// The node the shape sliders write to, or `None` when the selection no longer names one.
    pub fn selected_shape(&self) -> Option<&Shape> {
        shape_path::resolve(&self.entity_def.shape, &self.selected)
    }

    /// The mutable twin of [`EditorState::selected_shape`].
    pub fn selected_shape_mut(&mut self) -> Option<&mut Shape> {
        shape_path::resolve_mut(&mut self.entity_def.shape, &self.selected)
    }

    /// Select a node by path. Selecting a path that does not resolve is allowed, and reports itself as
    /// gone in the panel rather than being refused at the call site the list came from.
    pub fn select(&mut self, path: Vec<Step>) {
        self.selected = path;
    }

    /// Load entity from RON file
    pub fn load_ron(&mut self, path: &str) -> Result<(), String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path, e))?;

        let def: EntityDef =
            ron::from_str(&text).map_err(|e| format!("Failed to parse {}: {}", path, e))?;

        self.entity_def = def;
        // The tree the selection indexed into is gone; a path kept across a load would name some
        // unrelated node of the new animal and edit that.
        self.selected = Vec::new();
        // And the history belongs to the document that was open: an undo into a tree this file never
        // had would replace the animal the author just asked for.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.file_path = Some(path.to_string());
        self.dirty = false;
        Ok(())
    }

    /// Export current entity to RON file
    pub fn export_ron(&self, path: &str) -> Result<(), String> {
        let ron_text =
            ron::ser::to_string_pretty(&self.entity_def, ron::ser::PrettyConfig::default())
                .map_err(|e| format!("Failed to serialize: {}", e))?;

        std::fs::write(path, ron_text).map_err(|e| format!("Failed to write {}: {}", path, e))?;

        Ok(())
    }

    /// Mark entity as modified
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// What a preview of this entity would be made from: every value that reaches a pixel.
    ///
    /// `flags` and `quantity` are absent on purpose — they travel into the file but not into the image,
    /// so ticking "Alive" must not cost a march. `position`, `rotation`, `scale` and `material_id` are
    /// present because the viewport now reads all four: a key that left them out would keep showing the
    /// old frame while the inspector believed it had moved or recoloured the animal. So is the assigned
    /// slot's `base_color`, which is the one material field a shader reads — it lives in the registry
    /// rather than the entity, so it is the one part of the key that is not a field of `entity_def`.
    ///
    /// Floats go in as bits, so `-0.0` and `0.0` are not conflated and a NaN edit is seen as a change.
    /// Nothing hashed reads this: the editor is a tool, and the value only decides whether to re-march.
    pub fn preview_signature(&self) -> u64 {
        let def = &self.entity_def;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        ron::ser::to_string(&def.shape)
            .unwrap_or_default()
            .hash(&mut hasher);
        for value in def.position.iter().chain(def.rotation.iter()) {
            value.to_bits().hash(&mut hasher);
        }
        def.scale.to_bits().hash(&mut hasher);
        def.material_id.hash(&mut hasher);
        // The viewport multiplies the entity's pixels by the assigned slot's `base_color`, so a slot that
        // has been recoloured draws a different frame under the same `material_id`. Without these lanes
        // the key could not see a recolour at all — and until the MCP pipe could write a registry slot,
        // nothing could, so the gap was unreachable rather than harmless.
        if let Some(material) = self.materials.get(def.material_id) {
            for lane in material.base_color {
                lane.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::{Material, NoiseKind};

    #[test]
    fn the_preview_signature_reads_exactly_what_the_viewport_reads() {
        let mut state = EditorState::new();
        let base = state.preview_signature();

        // Each of these changes a pixel, so each must change the key — otherwise the editor keeps
        // showing the old frame while the inspector believes it moved or recoloured the animal.
        for edit in [
            |def: &mut EntityDef| def.position = [0.0, 0.5, 0.0],
            |def: &mut EntityDef| def.scale = 1.5,
            |def: &mut EntityDef| {
                def.rotation = [
                    0.0,
                    core::f32::consts::FRAC_1_SQRT_2,
                    0.0,
                    core::f32::consts::FRAC_1_SQRT_2,
                ]
            },
            |def: &mut EntityDef| def.material_id = 2,
            |def: &mut EntityDef| {
                def.shape = Shape::Sphere {
                    r: 0.5,
                    centre: [0.0, 0.0, 0.0],
                }
            },
        ] {
            edit(&mut state.entity_def);
            assert_ne!(
                state.preview_signature(),
                base,
                "an edit that reaches the frame left the preview key alone: {:?}",
                state.entity_def
            );
            state = EditorState::new();
        }

        // And the reverse: state that never reaches a pixel must not cost a march.
        state.entity_def.flags = 0x00;
        assert_eq!(state.preview_signature(), base, "flags are metadata");
        state.entity_def.quantity = 3;
        assert_eq!(state.preview_signature(), base, "quantity is metadata");

        // The registry is read by the same frame, so the column that matters is `shaded`, not "is this
        // field inside `entity_def`". A recoloured slot is a different picture under an unchanged
        // `material_id`; the other five fields are authored and read by nothing.
        let mut slot = state
            .materials
            .get(0)
            .copied()
            .expect("slot 0 is registered");
        slot.base_color = [0.0, 0.0, 0.0];
        state.materials.set(0, slot);
        assert_ne!(
            state.preview_signature(),
            base,
            "the viewport tints the entity with the assigned slot's base_color, so a recolour must \
             cost a march"
        );
        state.materials.set(0, Material::pig_skin());

        let mut slot = state
            .materials
            .get(0)
            .copied()
            .expect("slot 0 is registered");
        slot.roughness = 0.0;
        slot.emissive = 4.0;
        slot.index_ior = 2.4;
        slot.noise = NoiseKind::Perlin;
        slot.noise_frequency = 40.0;
        state.materials.set(0, slot);
        assert_eq!(
            state.preview_signature(),
            base,
            "five fields have no reader yet, so they must not each cost a march"
        );
    }

    #[test]
    fn a_selection_the_tree_no_longer_has_reports_itself_gone() {
        let mut state = EditorState::new();
        state.entity_def.shape = Shape::Union {
            a: Box::new(Shape::Sphere {
                r: 1.0,
                centre: [0.0, 0.0, 0.0],
            }),
            b: Box::new(Shape::Sphere {
                r: 0.4,
                centre: [0.8, 0.0, 0.0],
            }),
        };
        state.select(vec![Step::Second]);
        assert!(
            state.selected_shape().is_some(),
            "the part was there to select"
        );

        // Replace the tree the path indexed into with a leaf — what a load or a rebuild of the root
        // does. The old path now names nothing, and must not be quietly retargeted at the root.
        state.entity_def.shape = Shape::Sphere {
            r: 1.0,
            centre: [0.0, 0.0, 0.0],
        };
        assert!(
            state.selected_shape().is_none(),
            "a dead selection resolves through the root instead of reporting itself gone"
        );
        assert!(
            state.selected_shape_mut().is_none(),
            "a dead selection still handed the sliders something to write to"
        );
    }

    #[test]
    fn editing_a_part_through_the_selection_reaches_the_frame() {
        let mut state = EditorState::new();
        state.entity_def.shape = Shape::Union {
            a: Box::new(Shape::Sphere {
                r: 1.0,
                centre: [0.0, 0.0, 0.0],
            }),
            b: Box::new(Shape::Sphere {
                r: 0.4,
                centre: [0.8, 0.0, 0.0],
            }),
        };
        let base = state.preview_signature();

        state.select(vec![Step::Second]);
        match state.selected_shape_mut() {
            Some(Shape::Sphere { r, .. }) => *r = 0.75,
            other => panic!("selecting Union.b did not hand back a sphere: {other:?}"),
        }

        // The whole point of part selection: the viewport keys on the tree, so a part edit has to
        // re-march. A key that only read the root's own variant would keep showing the old snout.
        assert_ne!(
            state.preview_signature(),
            base,
            "an edit to a selected part left the preview key alone"
        );
    }
}
