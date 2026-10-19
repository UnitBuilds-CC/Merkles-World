//! 3D viewport with orbit camera for entity preview.

use egui::{Response, Sense, Vec2};

/// Orbit camera state for 3D viewport
pub struct ViewportCamera {
    /// Distance from target point
    pub distance: f32,
    /// Horizontal angle (radians)
    pub yaw: f32,
    /// Vertical angle (radians)
    pub pitch: f32,
    /// Target point to orbit around
    pub target: [f32; 3],
}

impl ViewportCamera {
    pub fn new() -> Self {
        Self {
            distance: 5.0,
            yaw: 0.0,
            pitch: 0.3,
            target: [0.0, 0.0, 0.0],
        }
    }

    /// Handle mouse drag for orbit
    pub fn handle_orbit(&mut self, delta: Vec2, sensitivity: f32) {
        self.yaw += delta.x * sensitivity;
        self.pitch = (self.pitch + delta.y * sensitivity).clamp(-1.5, 1.5);
    }

    /// Handle scroll wheel for zoom
    pub fn handle_zoom(&mut self, delta: f32) {
        self.distance *= 1.0 - delta * 0.1;
        self.distance = self.distance.max(0.5).min(50.0);
    }

    /// Get camera position in world space
    pub fn position(&self) -> [f32; 3] {
        let x = self.target[0] + self.distance * self.pitch.cos() * self.yaw.sin();
        let y = self.target[1] + self.distance * self.pitch.sin();
        let z = self.target[2] + self.distance * self.pitch.cos() * self.yaw.cos();
        [x, y, z]
    }

    /// Get forward direction vector
    pub fn forward(&self) -> [f32; 3] {
        let pos = self.position();
        let dx = self.target[0] - pos[0];
        let dy = self.target[1] - pos[1];
        let dz = self.target[2] - pos[2];
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        if len > 0.001 {
            [dx / len, dy / len, dz / len]
        } else {
            [0.0, 0.0, -1.0]
        }
    }
}

impl Default for ViewportCamera {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the 3D viewport panel
pub fn viewport_panel(ui: &mut egui::Ui, camera: &mut ViewportCamera) -> Response {
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::drag());

    // Handle mouse interaction
    if response.dragged() {
        let delta = response.drag_delta();
        camera.handle_orbit(Vec2::new(delta.x, -delta.y), 0.01);
    }

    // Draw background
    let rect = response.rect;
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(40, 40, 45));

    // Draw grid (simplified - would be replaced by actual GPU rendering)
    let center = rect.center();
    let grid_size = rect.width().min(rect.height()) * 0.4;

    // Draw axes
    painter.line_segment(
        [center, egui::pos2(center.x + grid_size, center.y)],
        (1.0, egui::Color32::RED),
    );
    painter.line_segment(
        [center, egui::pos2(center.x, center.y - grid_size)],
        (1.0, egui::Color32::GREEN),
    );

    // Draw info text
    painter.text(
        rect.left_top() + egui::vec2(10.0, 10.0),
        egui::Align2::LEFT_TOP,
        format!("Distance: {:.1}", camera.distance),
        egui::FontId::monospace(12.0),
        egui::Color32::WHITE,
    );

    response
}
