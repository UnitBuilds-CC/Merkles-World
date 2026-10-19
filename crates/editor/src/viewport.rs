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
    /// The opening framing. The raster's `half_height: 0.5` makes the vertical world span at distance
    /// `d` exactly `d`, and an authored creature is around one unit tall, so 5.0 — the value this
    /// carried while nothing was ever marched — showed a pig the size of a thumbnail. Scroll still
    /// covers 0.5 to 50, and `yaw` starts at π because an entity faces its own negative Z: at yaw 0 the
    /// editor opens on the animal's tail.
    pub fn new() -> Self {
        Self {
            distance: 2.0,
            yaw: core::f32::consts::PI,
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

    /// The camera's screen basis: `(forward, right, up)`, the same construction the raster uses in
    /// `gpu_renderer::create_orbit_camera`. Anything drawn as an overlay has to project through this or
    /// it disagrees with the image underneath it.
    pub fn basis(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        fn norm(v: [f32; 3]) -> [f32; 3] {
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if len > 1e-4 {
                [v[0] / len, v[1] / len, v[2] / len]
            } else {
                [0.0, 0.0, 1.0]
            }
        }
        fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        }
        let forward = self.forward();
        let right = norm(cross(forward, [0.0, 1.0, 0.0]));
        let up = norm(cross(right, forward));
        (forward, right, up)
    }

    /// Where a world-space direction points on screen, in egui coordinates (y grows downward).
    pub fn project_dir(&self, v: [f32; 3]) -> Vec2 {
        let (_, right, up) = self.basis();
        let dot = |a: [f32; 3]| a[0] * v[0] + a[1] * v[1] + a[2] * v[2];
        Vec2::new(dot(right), -dot(up))
    }
}

impl Default for ViewportCamera {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the 3D viewport panel.
///
/// This draws the frame and the interaction only. The stage grid and the orientation gizmo are not
/// drawn here: the grid is marched into the preview image so the entity can occlude it, and the gizmo
/// projects through [`ViewportCamera::project_dir`] so it cannot disagree with that image.
pub fn viewport_panel(ui: &mut egui::Ui, camera: &mut ViewportCamera) -> Response {
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::drag());

    // Handle mouse interaction
    if response.dragged() {
        let delta = response.drag_delta();
        camera.handle_orbit(Vec2::new(delta.x, -delta.y), 0.01);
    }

    // Draw background
    let rect = response.rect;
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(30, 30, 35));

    response
}
