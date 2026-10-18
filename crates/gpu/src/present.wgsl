// The presentation half: one full-screen triangle that turns the march's depth buffer into pixels.
//
// This file exists so `interp.wgsl` stays what it has always been - the interpreter, and nothing else. A
// render pass cannot bind a storage buffer through a compute pipeline's bind group, so the pass gets its own
// two bindings: the 16-byte extent block and the *same* `traced` buffer the compute entry point wrote. No
// copy, no intermediate texture, no read-back: the 8,294,400 bytes an R1 harness maps to the host are still
// on the device here, which is the only way a windowed frame can be cheaper than the headless one.
//
// What this shader is *not* is a material model. The colour is a linear ramp on the hit distance, and that
// is a deliberate refusal to invent: `render::raster` maps normals to rgb and names OQ-10 as the open
// question, and a normal on this path would cost six more tree walks per pixel - the very term gate R1 is
// out to pay for. Nothing here is hashed and nothing here is a simulation value (SPEC-02, ADR-017): the
// march law and the depths are the same bytes the headless instruments read.
//
// Row order is the buffer's own. `march_rays_from_camera` indexes `py * width + px` with `v = (py + 0.5) /
// height` and `ndc_y = (1 - 2v) * half_height`, so row 0 is the top of the picture; wgpu's fragment
// `position` is likewise measured from the top-left of the drawable. The indexing below is therefore direct,
// with no flip - and a flip would not be subtle: the demo's floor would arrive in the sky.

struct Extent {
    width: u32,
    height: u32,
    max_t: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> extent: Extent;
@group(0) @binding(1) var<storage, read> depths: array<f32>;

/// The no-surface test, as the host side already draws it: `interp.wgsl` writes `3.402823466e+38` for a ray
/// that finds nothing inside `max_t`, and `device.rs` counts a depth as a miss above `1.0e37`. Same
/// threshold, so a pixel this shader calls sky is a pixel `March::hits` did not count.
const MISS: f32 = 1.0e+37;

/// One triangle covering the viewport: the two off-screen corners are what makes three vertices enough, so
/// there is no index buffer and no second draw call.
@vertex
fn vs_full_screen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let cx = select(0u, 2u, index == 1u);
    let cy = select(0u, 2u, index == 2u);
    return vec4<f32>(f32(cx) - 1.0, f32(cy) - 1.0, 0.0, 1.0);
}

@fragment
fn fs_depth_ramp(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    let tx = min(u32(pixel.x), extent.width - 1u);
    let ty = min(u32(pixel.y), extent.height - 1u);
    let t = depths[ty * extent.width + tx];
    let sky = vec3<f32>(0.02, 0.03, 0.05);
    if (t >= MISS) {
        return vec4<f32>(sky, 1.0);
    }
    let near = vec3<f32>(0.92, 0.93, 0.95);
    let far = vec3<f32>(0.18, 0.22, 0.30);
    let k = clamp(t / extent.max_t, 0.0, 1.0);
    return vec4<f32>(mix(near, far, k), 1.0);
}
