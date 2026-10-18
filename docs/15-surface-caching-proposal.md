# 15 — Surface Caching Proposal (Draft)

**Status:** design exploration, not committed to roadmap
**Motivation:** Close R1's 5.5x gap without degrading resolution, scene density, or raising hardware requirements

## Problem

Current renderer marches every pixel from scratch each frame:
- 2,073,600 rays at 1080p
- 64-step cap costs 91.35 ms on GTX 1070
- Budget is 16.67 ms
- No temporal reuse between frames

Even when nothing changes, or when camera moves slightly, we re-march everything.

## Key Insight

Games have **temporal coherence**: consecutive frames are similar because:
- Camera moves smoothly (small deltas per frame)
- Entities move smoothly (pig walks forward, doesn't teleport)
- Most geometry is static (terrain, buildings)

If we remember where surfaces are, we can **transform cached data** instead of re-marching.

## Proposed Architecture

### 1. Entity Decomposition

Current: monolithic SDF tree
```rust
Tree {
    nodes: Vec<NodeRecord>,  // All shapes in one tree
    params: Vec<f32>,
}
```

Proposed: entity list with transforms
```rust
struct Entity {
    id: u64,
    sdf_subtree: Tree,       // This entity's shape
    transform: Transform,     // World position/orientation
    bounds: Enclosure,        // World-space bounding sphere
    version: u64,             // Incremented when entity changes
}

struct Scene {
    entities: Vec<Entity>,
    // No longer one big tree - separate subtrees
}
```

Each entity becomes independently trackable. A pig is "entity #42 at position (10, 0, 5)" rather than "nodes 15-23 in the tree".

### 2. World-Space Surface Cache

Instead of screen-space pixels, store surfaces indexed by world position:

```rust
struct SurfacePoint {
    world_pos: Vec3,          // Where in world space
    normal: Vec3,             // Surface orientation
    depth_from_camera: f32,   // For quick rejection
    entity_id: u64,           // Which entity this belongs to
    last_seen_frame: u64,     // For aging/invalidation
    confidence: f32,          // How well-marched this point is
}

struct SurfaceCache {
    // Spatial index for fast lookup
    spatial_grid: HashMap<VoxelKey, Vec<SurfacePoint>>,
    
    // Per-entity tracking
    entity_surfaces: HashMap<u64, Vec<SurfacePoint>>,
    
    // Frame counter for aging
    current_frame: u64,
    
    // Validity threshold - how far camera can move before invalidation
    max_camera_delta: f32,
}
```

### 3. Cache Update Strategy

When camera/entity moves:

**Old approach:** Re-march all 2M rays
**New approach:**
1. Check which entities moved significantly
2. For stationary entities: transform cached surfaces by camera delta
3. For moved entities: re-march only their bounding volumes
4. For new regions: march normally
5. Blend results seamlessly

```rust
fn update_cache(scene: &Scene, old_camera: Camera, new_camera: Camera) {
    let camera_delta = new_camera.origin - old_camera.origin;
    
    for entity in &scene.entities {
        let entity_moved = entity.transform.changed_significantly();
        
        if !entity_moved {
            // Transform cached surfaces instead of re-marching
            for point in cache.get_entity_points(entity.id) {
                point.world_pos -= camera_delta;  // Relative to new camera
                point.depth_from_camera = recalculate_depth(point, new_camera);
            }
        } else {
            // Re-march only this entity's bounds
            let new_surface = march_entity_bounds(entity, new_camera);
            cache.update_entity(entity.id, new_surface);
        }
    }
    
    // Invalidate points too far from new camera position
    cache.age_out_points(new_camera.origin);
}
```

### 4. Hybrid Rendering

Render loop combines cache + marching:

```rust
fn render_frame(scene: &Scene, camera: Camera, cache: &mut SurfaceCache) -> Frame {
    let mut frame = Frame::new(camera.width, camera.height);
    
    for pixel in frame.pixels() {
        let ray = camera.ray_for_pixel(pixel.x, pixel.y);
        
        // Try cache first (fast)
        if let Some(hit) = cache.try_hit(ray) {
            frame.set_pixel(pixel, hit.color);
            continue;
        }
        
        // Fall back to marching (slow but accurate)
        let hit = march_scene(scene, ray);
        frame.set_pixel(pixel, shade(hit));
        
        // Add to cache for next frame
        if let Some(hit) = hit {
            cache.add_point(hit.world_pos, hit.normal, hit.entity_id);
        }
    }
    
    return frame;
}
```

## Expected Benefits

### Work Reduction

Assuming typical gameplay:
- 80% of visible entities are stationary or slow-moving
- Camera moves ~1-2 units per frame (walking speed)
- Cache hit rate: 60-80% after first few frames

This means:
- First frame: 2M rays marched (91 ms)
- Subsequent frames: 400k-800k rays marched (18-36 ms)
- **Potential 2-5x speedup**, closing most of R1's gap

### Why This Works Better Than Pure Marching

1. **Marching is O(steps × complexity)** per ray
2. **Cache lookup is O(1)** per ray (spatial hash)
3. **Transform is O(1)** per cached point (matrix multiply)

For a scene with 100 entities:
- Marching: 2M rays × 64 steps × 100 entities = 12.8B operations
- Cached: 400k rays × 64 steps × 100 entities + 1.6M cache lookups = 2.6B operations
- **~5x reduction**

## Implementation Challenges

### 1. Entity Decomposition

Current code has no concept of entities. Need to:
- Modify `assetc` to emit per-entity trees
- Add entity ID tracking through render pipeline
- Ensure material/color functions work per-entity

**Difficulty:** Medium (architectural change, but straightforward)

### 2. World-Space Indexing

Need spatial structure that handles:
- Non-uniform entity sizes (pig vs terrain)
- Dynamic insertion/deletion (entities spawn/despawn)
- Fast range queries (what's near this ray?)

Options:
- Uniform grid (simple, good for voxel-sized entities)
- Octree (adaptive, better for varied sizes)
- BVH over cached points (optimal but expensive to rebuild)

**Recommendation:** Start with uniform grid at 1-unit resolution, match voxel size

### 3. Cache Coherence

Problem: transformed cache points may not align perfectly with actual surface
- Floating-point drift over many frames
- Non-linear transforms (rotation, scale) break simple translation

Solution:
- Periodic re-validation: remarch 10% of cached points each frame
- Confidence metric: age out points that haven't been verified recently
- Accept small errors: cache is presentation-only (docs/02 float discipline)

### 4. Blending Artifacts

Where cache meets newly-marched regions, visible seams could appear:
- Different surface normals at boundary
- Depth discontinuities

Solution:
- Overlap cache/march regions slightly (cache "plus a little around the edge")
- Smooth blend in overlap zone
- Or: accept artifacts as cheaper than full remarch (game aesthetic choice)

## Alternative: Screen-Space Cache (Rejected)

Initial prototype (`crates/render/src/surface_cache.rs`) used screen-space indexing:
- Store surface points by pixel coordinates
- Look up by screen position

**Why it failed:** When camera moves, same world surface appears at different screen coordinates. Hit rate dropped to 0% after first frame.

World-space indexing is essential because surfaces exist independently of the camera.

## Next Steps (If Pursued)

1. **Prototype entity decomposition**
   - Modify boot scene to use 3 separate entities instead of one tree
   - Add per-entity transform tracking
   
2. **Build world-space cache**
   - Implement spatial grid indexer
   - Test cache lookup accuracy
   
3. **Measure cache hit rates**
   - Simulate camera movement through test scene
   - Quantify work reduction vs pure marching
   
4. **Integrate with GPU path**
   - Upload cache as storage buffer
   - Modify WGSL interpreter to check cache before marching

## Risks

1. **Architectural overhaul:** Major change to how scenes are represented
2. **Memory cost:** Cache needs RAM/VRAM for surface points
3. **Complexity:** More state to manage, more failure modes
4. **May not close gap:** Even 80% cache hit rate leaves 20% marching, which might still exceed budget

## Decision Point

This proposal requires significant architectural changes. Before committing:

- Is entity decomposition compatible with doc 09's asset pipeline?
- Does this violate "no art" principle (caches are derived, not authored)?
- Is the complexity worth the potential 2-5x speedup?

**Recommendation:** Build minimal prototype measuring cache hit rates before full implementation.
