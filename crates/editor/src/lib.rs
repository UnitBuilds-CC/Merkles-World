//! Entity editor: visual SDF authoring tool with an egui UI.
//!
//! This is a **tool** (AGENTS.md §3), not part of the runtime hot path. It enables:
//! - A viewport that marches the entity with `crates/render`'s CPU reference — the same evaluator the
//!   WGSL interpreter has to agree with. Not the GPU: eframe owns the window's device, `gpu::Interpreter`
//!   owns its own, and no temporal cache is wired into this path yet.
//! - Editing the authored transform (`position`, `scale`) and the root node's own parameters. A node
//!   deeper in the tree is shown in the image but is not yet selectable — docs/16 § What the editor does
//!   not do yet.
//! - Material slot assignment and a swatch read out of the same registry the viewport shades with
//! - RON load and save, with dirty tracking
//!
//! Usage: `cargo run -p editor --example entity_editor`, or `--example render_png` to write the same
//! frames out when there is no one to look at a window.

pub mod export;
pub mod gpu_renderer;
pub mod inspector;
pub mod material_panel;
pub mod viewport;

use core::hash::{Hash, Hasher};

use assetc::{EntityDef, Shape};
use entity::MaterialRegistry;

/// Editor state: tracks the currently loaded entity and dirty flag
pub struct EditorState {
    /// Current entity definition being edited
    pub entity_def: EntityDef,
    /// The materials a `material_id` may point at. One registry, read by both the panel's swatches
    /// and the viewport's shading, so the two cannot disagree about what slot 0 looks like.
    pub materials: MaterialRegistry,
    /// Whether parameters have been modified since last save
    pub dirty: bool,
    /// Path to loaded file (if any)
    pub file_path: Option<String>,
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
            dirty: false,
            file_path: None,
        }
    }

    /// Load entity from RON file
    pub fn load_ron(&mut self, path: &str) -> Result<(), String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path, e))?;

        let def: EntityDef =
            ron::from_str(&text).map_err(|e| format!("Failed to parse {}: {}", path, e))?;

        self.entity_def = def;
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

    /// What a preview of this entity would be made from: every authored value that reaches a pixel.
    ///
    /// `flags` and `quantity` are absent on purpose — they travel into the file but not into the image,
    /// so ticking "Alive" must not cost a march. `position`, `rotation`, `scale` and `material_id` are
    /// present because the viewport now reads all four: a key that left them out would keep showing the
    /// old frame while the inspector believed it had moved or recoloured the animal.
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
    }
}
