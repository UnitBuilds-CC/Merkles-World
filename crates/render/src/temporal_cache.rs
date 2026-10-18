//! Temporal surface cache: reuse rendered surfaces across frames by transforming cached data
//! instead of remarching.
//!
//! The idea: consecutive frames are highly coherent. When the camera moves slightly, most pixels
//! still show the same surfaces at nearly the same world positions. Instead of remarching every
//! ray, we:
//! 1. Store per-pixel surface data from frame N (world position, depth, normal)
//! 2. For frame N+1: transform cached data by camera delta
//! 3. Validate with quick SDF check (is cached point still on surface?)
//! 4. Only remarch pixels where validation fails
//!
//! This turns rendering from "march all pixels" into "validate most, march few".

use crate::{sdf_eval, raster::Camera, Tree, Vec3, ROOT};

/// Cached surface data for one pixel
#[derive(Debug, Clone, Copy)]
pub struct CachedPixel {
    /// World-space position where surface was found (zero for sky pixels)
    pub world_pos: Vec3,
    /// Distance from camera origin to surface along ray, or f32::MAX for sky
    pub depth: f32,
    /// Surface normal at hit point (zero for sky pixels)
    pub normal: Vec3,
    /// Which frame this came from (for aging/invalidation)
    pub frame_id: u64,
    /// For sky pixels: the maximum t the ray reached before giving up
    pub max_t: f32,
}

impl CachedPixel {
    /// Sky pixel marker - no surface found
    pub fn sky(max_t: f32) -> Self {
        Self {
            world_pos: [0.0, 0.0, 0.0],
            depth: f32::MAX,
            normal: [0.0, 0.0, 0.0],
            frame_id: 0,
            max_t,
        }
    }

    pub fn is_sky(&self) -> bool {
        self.depth == f32::MAX
    }
}

/// Frame-to-frame surface cache
pub struct SurfaceCache {
    /// Per-pixel cached data, row-major
    pixels: Vec<CachedPixel>,
    width: u32,
    height: u32,
    /// Current frame counter
    frame_id: u64,
    /// Camera from last frame (for computing delta)
    last_camera: Option<Camera>,
}

impl SurfaceCache {
    pub fn new(width: u32, height: u32) -> Self {
        let total = (width * height) as usize;
        Self {
            pixels: vec![CachedPixel::sky(40.0); total], // Default max_t for initialization
            width,
            height,
            frame_id: 0,
            last_camera: None,
        }
    }

    /// Check if a cached pixel is still valid given new camera position
    /// Returns true if the cached surface is still approximately correct
    pub fn validate_pixel(
        &self,
        tree: &Tree,
        cached: &CachedPixel,
        _new_camera_origin: Vec3,
        surface_eps: f32,
    ) -> bool {
        if cached.is_sky() {
            return false; // Sky pixels use separate validation below
        }

        // Quick validation: is the cached world position still on/near a surface?
        // Use generous tolerance since camera moved and exact hit point may shift
        let tolerance = surface_eps * 100.0; // Very generous - we just want to know if surface exists here
        let dist = sdf_eval(tree, ROOT, cached.world_pos);
        dist.abs() < tolerance
    }

    /// Validate a cached sky pixel: check if the ray still misses with the new camera
    pub fn validate_sky_pixel(
        &self,
        enclosure: crate::cull::Enclosure,
        origin: Vec3,
        dir: Vec3,
        max_t: f32,
    ) -> bool {
        // If the scene has no bounded enclosure, we can't prove anything - must remarch
        if matches!(enclosure, crate::cull::Enclosure::Unbounded) {
            return false;
        }

        // Check if the ray would still miss the scene bounds
        !crate::cull::ray_may_hit(enclosure.grown(max_t * 0.01), origin, dir, max_t)
    }

    /// Render with temporal caching: reuse valid cached pixels, remarch invalid ones
    /// Returns (frame_stats, cache_hit_count)
    pub fn render_with_cache(
        &mut self,
        tree: &Tree,
        camera: &Camera,
        surface_fn: impl Fn(&Tree, Vec3, Vec3) -> Option<(f32, Vec3, Vec3)>,
    ) -> (usize, usize) {
        let width = self.width;
        let height = self.height;
        let mut hits = 0usize;
        let mut cache_hits = 0usize;

        self.frame_id += 1;

        for py in 0..height {
            for px in 0..width {
                let idx = (py * width + px) as usize;
                let cached = &self.pixels[idx];

                // Try to validate cached data
                if cached.frame_id > 0 && self.validate_pixel(tree, cached, camera.origin, camera.surface) {
                    // Cache is valid! Reuse without marching
                    cache_hits += 1;
                    hits += 1;
                    // Update frame_id to keep this pixel fresh
                    self.pixels[idx].frame_id = self.frame_id;
                    continue;
                }

                // Cache invalid or missing - must remarch

                // Compute ray direction for this pixel
                let dir = ray_for_pixel(camera, px, py);

                // March the ray
                match surface_fn(tree, camera.origin, dir) {
                    Some((t, hit_point, normal)) => {
                        hits += 1;
                        self.pixels[idx] = CachedPixel {
                            world_pos: hit_point,
                            depth: t,
                            normal,
                            frame_id: self.frame_id,
                            max_t: 0.0, // Not used for surface pixels
                        };
                    }
                    None => {
                        // Still sky
                        self.pixels[idx] = CachedPixel::sky(camera.max_t);
                    }
                }
            }
        }

        self.last_camera = Some(*camera);
        (hits, cache_hits)
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        let total = self.pixels.len();
        let valid = self.pixels.iter().filter(|p| p.frame_id > 0 && !p.is_sky()).count();
        let sky = self.pixels.iter().filter(|p| p.is_sky()).count();
        let aged = self.pixels.iter().filter(|p| p.frame_id > 0 && p.frame_id < self.frame_id.saturating_sub(10)).count();

        CacheStats {
            total_pixels: total,
            cached_surfaces: valid,
            sky_pixels: sky,
            aged_pixels: aged,
            current_frame: self.frame_id,
        }
    }

    /// Get cached pixel data by index
    pub fn get_cached(&self, index: usize) -> Option<CachedPixel> {
        if index < self.pixels.len() && self.pixels[index].frame_id > 0 {
            Some(self.pixels[index])
        } else {
            None
        }
    }

    /// Mark a cached pixel as used in current frame (keep it fresh)
    pub fn mark_used(&mut self, index: usize) {
        if index < self.pixels.len() {
            self.pixels[index].frame_id = self.frame_id;
        }
    }

    /// Cache a surface hit at the given pixel index
    pub fn cache_surface(&mut self, index: usize, world_pos: Vec3, depth: f32, normal: Vec3) {
        if index < self.pixels.len() {
            self.pixels[index] = CachedPixel {
                world_pos,
                depth,
                normal,
                frame_id: self.frame_id,
                max_t: 0.0, // Not used for surface pixels
            };
        }
    }

    /// Cache a sky pixel (no surface found)
    pub fn cache_sky(&mut self, index: usize, max_t: f32) {
        if index < self.pixels.len() {
            self.pixels[index] = CachedPixel {
                world_pos: [0.0, 0.0, 0.0],
                depth: f32::MAX,
                normal: [0.0, 0.0, 0.0],
                frame_id: self.frame_id,
                max_t,
            };
        }
    }

    /// Get current frame counter
    pub fn current_frame(&self) -> u64 {
        self.frame_id
    }

    /// Advance to next frame (call at start of each render)
    pub fn advance_frame(&mut self) {
        self.frame_id += 1;
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_pixels: usize,
    pub cached_surfaces: usize,
    pub sky_pixels: usize,
    pub aged_pixels: usize,
    pub current_frame: u64,
}

fn ray_for_pixel(camera: &Camera, px: u32, py: u32) -> Vec3 {
    use crate::raster;
    let forward = raster::unit(camera.forward);
    let right = raster::unit(camera.right);
    let up = raster::unit(camera.up);

    let aspect = camera.width as f32 / camera.height as f32;
    let u = (px as f32 + 0.5) / camera.width as f32;
    let v = (py as f32 + 0.5) / camera.height as f32;
    let ndc_x = (2.0 * u - 1.0) * camera.half_height * aspect;
    let ndc_y = (1.0 - 2.0 * v) * camera.half_height;

    raster::unit([
        forward[0] + right[0] * ndc_x + up[0] * ndc_y,
        forward[1] + right[1] * ndc_x + up[1] * ndc_y,
        forward[2] + right[2] * ndc_x + up[2] * ndc_y,
    ])
}
