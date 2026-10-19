//! CPU renderer for viewport preview - renders SDF shapes to display in editor.
//!
//! This module uses the CPU raster to provide immediate visual feedback when editing
//! entity parameters, without requiring full GPU pipeline integration.

use assetc::{EntityDef, Shape};
use render::{raster::{Camera, Frame}, Tree, Vec3, ROOT, march, normal};

/// CPU renderer that converts entity shapes to pixel previews
pub struct CpuRenderer {
    width: u32,
    height: u32,
}

impl CpuRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Render the current entity shape to pixels
    pub fn render(&self, entity_def: &EntityDef, camera_yaw: f32, camera_pitch: f32, camera_distance: f32) -> Frame {
        // Convert entity to tree
        let tree = entity_to_tree(entity_def);
        
        // Create camera at orbit position
        let camera = create_orbit_camera(camera_yaw, camera_pitch, camera_distance, self.width, self.height);
        
        // Render the frame
        render_frame(&tree, &camera)
    }

    /// Resize the renderer
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }
}

/// Convert an EntityDef to a renderable Tree (recursive, handles full shape hierarchy)
fn entity_to_tree(entity_def: &EntityDef) -> Tree {
    use contracts::sdf::{pack_kind_and_arity, pack_first_and_flags, pack_params_len, FLAG_IS_ROOT};
    
    // First pass: collect all nodes and params in pre-order traversal
    let mut nodes = Vec::new();
    let mut params = Vec::new();
    
    // Start recursive conversion with root flag
    convert_shape_recursive(&entity_def.shape, &mut nodes, &mut params, true);
    
    Tree { nodes, params }
}

/// Recursively convert a Shape into NodeRecords and parameters
fn convert_shape_recursive(
    shape: &assetc::Shape,
    nodes: &mut Vec<contracts::sdf::NodeRecord>,
    params: &mut Vec<f32>,
    is_root: bool,
) {
    use contracts::sdf::{pack_kind_and_arity, pack_first_and_flags, pack_params_len, FLAG_IS_ROOT};
    
    let flags = if is_root { FLAG_IS_ROOT } else { 0 };
    
    match shape {
        assetc::Shape::Sphere { r, centre } => {
            let params_first = params.len() as u16;
            params.extend_from_slice(&[*r, centre[0], centre[1], centre[2]]);
            
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_SPHERE, 0),
                1, // subtree_len for leaf
                pack_first_and_flags(params_first, flags),
                pack_params_len(4),
            ]));
        }
        assetc::Shape::RoundedBox { half, r } => {
            let params_first = params.len() as u16;
            params.extend_from_slice(&[half[0], half[1], half[2], *r]);
            
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_ROUNDED_BOX, 0),
                1,
                pack_first_and_flags(params_first, flags),
                pack_params_len(4),
            ]));
        }
        assetc::Shape::SmoothUnion { a, b, k } => {
            let params_first = params.len() as u16;
            params.push(*k);
            
            // Placeholder node - we'll update subtree_len after processing children
            let node_index = nodes.len();
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_SMOOTH_UNION, 2),
                0, // Will be updated after children are processed
                pack_first_and_flags(params_first, flags),
                pack_params_len(1),
            ]));
            
            // Process children
            convert_shape_recursive(a, nodes, params, false);
            convert_shape_recursive(b, nodes, params, false);
            
            // Update subtree_len (this node + all descendants)
            let total_nodes = nodes.len() - node_index;
            nodes[node_index].0[1] = total_nodes as u32;
        }
        assetc::Shape::Union { a, b } => {
            let params_first = params.len() as u16;
            
            let node_index = nodes.len();
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_UNION, 2),
                0,
                pack_first_and_flags(params_first, flags),
                pack_params_len(0),
            ]));
            
            convert_shape_recursive(a, nodes, params, false);
            convert_shape_recursive(b, nodes, params, false);
            
            let total_nodes = nodes.len() - node_index;
            nodes[node_index].0[1] = total_nodes as u32;
        }
        assetc::Shape::Subtract { kept, cutter } => {
            let params_first = params.len() as u16;
            
            let node_index = nodes.len();
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_SUBTRACT, 2),
                0,
                pack_first_and_flags(params_first, flags),
                pack_params_len(0),
            ]));
            
            convert_shape_recursive(kept, nodes, params, false);
            convert_shape_recursive(cutter, nodes, params, false);
            
            let total_nodes = nodes.len() - node_index;
            nodes[node_index].0[1] = total_nodes as u32;
        }
        assetc::Shape::Intersect { a, b } => {
            let params_first = params.len() as u16;
            
            let node_index = nodes.len();
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_INTERSECT, 2),
                0,
                pack_first_and_flags(params_first, flags),
                pack_params_len(0),
            ]));
            
            convert_shape_recursive(a, nodes, params, false);
            convert_shape_recursive(b, nodes, params, false);
            
            let total_nodes = nodes.len() - node_index;
            nodes[node_index].0[1] = total_nodes as u32;
        }
        _ => {
            // Fallback: unsupported shape type, use a sphere
            let params_first = params.len() as u16;
            params.extend_from_slice(&[1.0, 0.0, 0.0, 0.0]);
            
            nodes.push(contracts::sdf::NodeRecord([
                pack_kind_and_arity(contracts::sdf::NODE_SPHERE, 0),
                1,
                pack_first_and_flags(params_first, flags),
                pack_params_len(4),
            ]));
        }
    }
}

/// Create an orbit camera looking at origin
fn create_orbit_camera(yaw: f32, pitch: f32, distance: f32, width: u32, height: u32) -> Camera {
    // Calculate camera position from spherical coordinates
    let x = distance * pitch.cos() * yaw.sin();
    let y = distance * pitch.sin();
    let z = distance * pitch.cos() * yaw.cos();

    let position = [x, y, z];
    
    // Calculate forward direction (towards origin)
    let mut forward = normalize([
        -position[0],
        -position[1],
        -position[2],
    ]);
    
    // World up vector
    let world_up = [0.0, 1.0, 0.0];
    
    // Calculate right and up vectors
    let right = normalize(cross(forward, world_up));
    let up = cross(right, forward);
    
    // Normalize up
    let up = normalize(up);

    Camera {
        origin: position,
        forward,
        right,
        up,
        half_height: 0.5,
        width,
        height,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

/// Render a tree with given camera
fn render_frame(tree: &Tree, camera: &Camera) -> Frame {
    let mut frame = Frame::new(camera.width, camera.height);
    
    // March all rays
    for y in 0..camera.height {
        for x in 0..camera.width {
            // Calculate ray direction
            let aspect = camera.width as f32 / camera.height as f32;
            let px = (x as f32 / camera.width as f32 - 0.5) * aspect * camera.half_height * 2.0;
            let py = (0.5 - y as f32 / camera.height as f32) * camera.half_height * 2.0;
            
            let dir = normalize([
                camera.forward[0] + camera.right[0] * px + camera.up[0] * py,
                camera.forward[1] + camera.right[1] * px + camera.up[1] * py,
                camera.forward[2] + camera.right[2] * px + camera.up[2] * py,
            ]);
            
            // March the ray
            if let Some(hit) = march(tree, ROOT, camera.origin, dir, camera.max_t, camera.surface, camera.max_steps) {
                // Hit something - calculate normal and color
                let n = normal(tree, ROOT, hit.point, camera.surface);
                
                // Convert normal to RGB color
                frame.pixels.extend_from_slice(&[
                    ((n[0] * 0.5 + 0.5) * 255.0) as u8,
                    ((n[1] * 0.5 + 0.5) * 255.0) as u8,
                    ((n[2] * 0.5 + 0.5) * 255.0) as u8,
                ]);
                
                frame.depth.push(hit.t);
            } else {
                // No hit - sky color (dark gray)
                frame.pixels.extend_from_slice(&[30, 30, 35]);
                frame.depth.push(Frame::NO_HIT);
            }
        }
    }
    
    frame
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0001 {
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

