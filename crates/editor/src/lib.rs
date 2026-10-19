//! Entity editor: visual SDF authoring tool with Dear ImGui UI.
//!
//! This is a **tool** (AGENTS.md §3), not part of the runtime hot path. It enables:
//! - Real-time GPU rendering of entity shapes with temporal cache
//! - Interactive parameter editing via ImGui widgets
//! - SDF tree hierarchy visualization
//! - Material slot assignment and preview
//! - RON export for Phase 2 entity framework integration
//!
//! Usage: `cargo run -p editor --example entity_editor`

pub mod viewport;
pub mod inspector;
pub mod material_panel;
pub mod export;
pub mod gpu_renderer;

use assetc::{EntityDef, Shape};

/// Editor state: tracks the currently loaded entity and dirty flag
pub struct EditorState {
    /// Current entity definition being edited
    pub entity_def: EntityDef,
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
            dirty: false,
            file_path: None,
        }
    }

    /// Load entity from RON file
    pub fn load_ron(&mut self, path: &str) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path, e))?;

        let def: EntityDef = ron::from_str(&text)
            .map_err(|e| format!("Failed to parse {}: {}", path, e))?;

        self.entity_def = def;
        self.file_path = Some(path.to_string());
        self.dirty = false;
        Ok(())
    }

    /// Export current entity to RON file
    pub fn export_ron(&self, path: &str) -> Result<(), String> {
        let ron_text = ron::ser::to_string_pretty(
            &self.entity_def,
            ron::ser::PrettyConfig::default(),
        )
        .map_err(|e| format!("Failed to serialize: {}", e))?;

        std::fs::write(path, ron_text)
            .map_err(|e| format!("Failed to write {}: {}", path, e))?;

        Ok(())
    }

    /// Mark entity as modified
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}
