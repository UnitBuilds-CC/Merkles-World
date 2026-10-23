//! The device half: upload a shape tree exactly as `assetc` wrote it, run A-07's WGSL master interpreter
//! over a set of points, and compare the distances with the CPU reference in `crates/render`.
//!
//! This is the check docs/04 has been promising since ADR-012: the grammar has two implementations, and
//! "the shader agrees with the interpreter" is worth nothing until somebody runs both and subtracts. WGSL
//! has no recursion, so the shader walks the tree with an explicit stack while the reference recurses -
//! two different ways to get the same answer, which is exactly the situation a contract should be nervous
//! about.
//!
//! What agreement would mean, and what it would not: it is evidence about presentation code agreeing with
//! presentation code. Nothing here is hashed, so no number in this file may be quoted as gate P1.5 or
//! gate R4 evidence (ADR-017), and running on a software adapter says nothing about gate R1's frame
//! budget. The `f32` tolerance below is a statement about two different evaluators, not a claim that the
//! renderer is deterministic across machines - the march's step counts already are, and were measured to
//! be so, in `raster_probe`.
//!
//! No device is a legitimate outcome: the x86-64 Linux CI runner reports no adapters at all, so
//! [`Interpreter::open`] distinguishes "no adapter" from "adapter that would not open" and the caller
//! prints a skip rather than a failure.

use core::cell::RefCell;
use core::ops::Range;
use render::{sdf_eval, Ray, Tree, Vec3, ROOT};
use wgpu::util::DeviceExt;

/// The stack capacity declared in `interp.wgsl`'s `DEPTH`, kept in sync by the test that asserts both.
pub const STACK_DEPTH: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Nothing enumerated an adapter. The Linux CI runner is the case this exists for.
    NoAdapter,
    /// An adapter existed and refused a device - a real failure, never to be reported as a skip.
    NoDevice,
}

pub struct Interpreter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Kept because a surface is only usable with the instance and adapter it was created against: dropping
    /// the instance that enumerated the adapter would leave a window with no device to configure it against.
    /// Both are refcounted inside wgpu, so this costs two `Arc` clones at `open` and nothing per frame.
    instance: wgpu::Instance,
    window_adapter: wgpu::Adapter,
    pipeline: wgpu::ComputePipeline,
    march_pipeline: wgpu::ComputePipeline,
    camera_pipeline: wgpu::ComputePipeline,
    counted_pipeline: wgpu::ComputePipeline,
    adapter: String,
}

/// The camera-path pipeline compiled from the shipped `interp.wgsl` with its `DEPTH` constant replaced, so the
/// interpreter's per-invocation stack capacity is a measured term instead of an argument.
///
/// This is a *measurement* device, not a second interpreter: the source text is the same file and the only bytes
/// that change are the literal on the `const DEPTH` line, so the march law, the walk, the combine arms and every
/// binding are exactly the ones the shipped pipeline compiles. Duplicating the walk to vary its stack would have
/// been the fork `interp.wgsl`'s header comment exists to prevent.
///
/// Kept opaque so `wgpu::ComputePipeline` stays out of the crate's public surface. Note what a smaller capacity
/// costs in correctness rather than milliseconds: the walk's own guard returns the no-surface sentinel once the
/// traversal exceeds it, so a variant below the loaded tree's depth is not a faster renderer, it is a refusal.
pub struct CameraVariant {
    pub depth: u32,
    pipeline: wgpu::ComputePipeline,
    /// Whether this pipeline's own entry point reaches the `@group(1)` temporal cache, read off
    /// `interp.wgsl` when the pipeline was compiled. wgpu derives a `layout: None` pipeline's layouts from the
    /// resources the entry point references, so this is a property of the shader and not a choice: binding a
    /// cache group on a pipeline that never derived one is a validation error on the device.
    temporal_cache: bool,
}

/// One timed bench pass. Two modes, because queueing work and waiting for it are different costs.
#[derive(Debug, Clone)]
pub struct Bench {
    pub adapter: String,
    pub points: usize,
    pub dispatches: usize,
    pub evaluations: usize,
    pub ms_submit_only_total: f64,
    pub ms_per_pass_with_read: f64,
    /// `dispatches` passes queued together and retrieved with a single copy.
    pub ms_batch_with_single_read: f64,
}

/// One frame marched entirely on the device. `frame_curve`'s columns price a harness that carries a grid
/// back and forth; this prices the thing gate R1 names, where the ray loop is the shader and the only
/// bytes crossing the bus are the depths at the end.
#[derive(Debug, Clone)]
pub struct March {
    pub adapter: String,
    pub rays: usize,
    pub max_steps: u32,
    pub surface: f32,
    /// Rays that returned a `t`; the rest carry the no-surface sentinel.
    pub hits: usize,
    /// Handing the dispatch to the queue, which is not a frame time.
    pub ms_submit_only: f64,
    /// Dispatch plus the copy-and-map: the interval a caller actually waits on.
    pub ms_frame_with_read: f64,
    pub depths: Vec<f32>,
}

/// A march, plus the arm's tally as the device itself counted it.
///
/// `March` alone cannot answer "did the bound-skip arm do anything on a GPU": the depths are the same by
/// design, since a skip is only sound when the picture is unchanged. This carries the one integer that
/// distinguishes an arm that fires from an arm that never fires — per ray, so a scene where only a few
/// rays pass a union's second child reads differently from one where all of them do.
#[derive(Debug, Clone)]
pub struct Counted {
    pub march: March,
    /// Union second children this ray's walk refused to evaluate, one entry per ray in ray order.
    pub skips: Vec<u32>,
}

/// One read-back, priced on its own: the copy of `pixels * 4` depth bytes and the map that makes them
/// readable, with no dispatch in the interval. Paired with `march_camera_frame_no_readback` to split an R1
/// frame's last two terms once the ray bytes are gone.
#[derive(Debug, Clone)]
pub struct ReadBack {
    /// Bytes the copy actually moved, read off the copy this call made rather than computed by the caller.
    pub bytes: usize,
    /// Rays that carry a `t` in the buffer as it was read.
    pub hits: usize,
    pub depths: Vec<f32>,
    /// Copy plus map — what a caller waiting on the frame's pixels pays after the march itself.
    pub ms: f64,
}

/// The bytes that go into a shape's `pool` buffer: the tree's parameters, then the per-node bound table.
///
/// The tail is `render::cull::SceneBounds::encoded()` — `[centre.x, centre.y, centre.z, radius]` per node,
/// four `f32` each, in node order. It shares the parameter buffer because `interp.wgsl` already binds four
/// storage buffers, which is `wgpu::Limits::downlevel_defaults()`' ceiling, and a per-node table will not
/// fit in the header uniform; the shader therefore finds the tail by
/// `arrayLength(&pool) - 4 * arrayLength(&nodes)`. That is also why the tail is **not optional**: a pool
/// with no table would have that arithmetic read parameter words as radii, and parameter words are
/// finite. Nodes the cull table refuses to license arrive as `+inf`, which bounds nothing.
///
/// The table is derived from the same `&Tree` the parameters come from, so no caller can supply a picture
/// of the scene that does not match the scene it uploads — and no caller has to remember to.
fn pool_bytes(tree: &Tree) -> Vec<u8> {
    let mut out = bytes_of_f32(&tree.params);
    for row in render::cull::scene_bounds(tree).encoded() {
        out.extend_from_slice(&vec4_bytes(row));
    }
    out
}

/// A shape tree resident on the device: the two buffers the march binds as slots 0 and 1, created once and
/// handed to every frame after that. `march` re-uploads them per call because one upload per frame is the
/// shape a correctness probe wants; gate AS4's clause is different — a frame must not allocate or parse its
/// way to the queue — and a re-uploaded asset allocates and copies even though it parsed nothing. `bytes` is
/// what the per-frame path pays again on every frame after the first.
#[derive(Debug)]
pub struct Shape {
    nodes: wgpu::Buffer,
    pool: wgpu::Buffer,
    pub bytes: usize,
    pub node_count: usize,
    /// The capacity each buffer was created with, so a re-upload can refuse a tree that outgrows the shape
    /// instead of writing past it. AS4's asset-side clause: an edit reuses these bytes and creates nothing.
    pub nodes_capacity: u64,
    pub pool_capacity: u64,
}

/// A frame's device-side buffers, created once for a fixed ray count and refilled by `march_frame`.
///
/// The host-side allocation this replaces is four buffers plus a bind group per march; on this adapter that
/// churn is not just a gate violation, it is what lost the device at 1080p. `packed` is the ray byte buffer
/// the frame writes into in place, so refilling a frame parses nothing and allocates nothing either.
#[derive(Debug)]
pub struct Frame {
    pub rays: usize,
    queries: wgpu::Buffer,
    header: wgpu::Buffer,
    traced: wgpu::Buffer,
    staging: wgpu::Buffer,
    packed: Vec<u8>,
    /// The step cap and surface tolerance currently on the device for this frame, recorded by `write_rays`.
    /// A frame with no law has never been filled, and marching it would report numbers for rays that are not
    /// there.
    law: Option<(u32, f32)>,
}

/// The bind group pairing one resident `Shape` with one `Frame`. AS4's frame-side clause counts bind groups
/// as well as buffers, so this is created per (shape, frame) pair and reused by every frame after the first.
#[derive(Debug)]
pub struct Bind {
    bind: wgpu::BindGroup,
    /// Optional second bind group for temporal cache buffers (desktop only).
    cache_bind: Option<wgpu::BindGroup>,
    /// Dirty mask buffer for delta rendering - allows CPU-side invalidation of cached pixels.
    dirty_mask_buf: Option<wgpu::Buffer>,
    /// The host's copy of the dirty mask, word for word.
    ///
    /// `write_buffer` replaces, it cannot OR, so without a mirror of what the device already holds a second
    /// region landing in a word the first one touched would clear that word's earlier bits. The mirror can
    /// only ever *over*-report dirt, and the shader clears every bit it reads (`interp.wgsl`'s culled camera
    /// entry), so a submitted cached frame leaves the device mask empty and this is reset with it. A miss of
    /// that reset costs a re-march, never a stale pixel.
    dirty_shadow: RefCell<Vec<u32>>,
    /// Cache statistics buffer [cache_hits, total_dirty_checked, reserved, reserved].
    cache_stats_buf: Option<wgpu::Buffer>,
    /// Width and height for calculating pixel indices from rectangles.
    width: u32,
    height: u32,
}

/// A frame whose rays the device generates itself, from a camera block instead of a ray list.
///
/// The difference from `Frame` is the absence of `packed` and `queries`: at 1920x1080 those are
/// 66,355,200 bytes the host builds and pushes down the bus every frame, which `r1_step_price` measured as
/// the fixed term that puts a single step over 16.67 ms. What remains is a 16-byte header, a 96-byte camera,
/// and the depths coming back.
#[derive(Debug)]
pub struct CameraFrame {
    pub pixels: usize,
    pub width: u32,
    pub height: u32,
    header: wgpu::Buffer,
    camera: wgpu::Buffer,
    traced: wgpu::Buffer,
    staging: wgpu::Buffer,
    /// The step cap and surface tolerance `write_camera` last put on the device for this frame, so a march
    /// cannot report a law the frame never received.
    law: Option<(u32, f32)>,
}

/// The render pass that puts a marched frame's depths on a window, made once per (frame, surface format).
///
/// AS4's frame-side clause counts what a frame creates, and a frame that rebuilds a pipeline, a bind group and
/// a uniform every 16 ms is the churn this crate has been deleting one buffer at a time. `draw` creates one
/// command encoder — which is the shape of a wgpu frame, not an allocation of ours — and touches no buffer:
/// the fragment stage reads the compute stage's storage buffer in place, so the depth bytes never leave the
/// device. Kept opaque for the same reason as `CameraVariant`: `wgpu::RenderPipeline` stays out of the API.
#[derive(Debug)]
pub struct Present {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    extent: wgpu::Buffer,
    depth_bytes: u64,
}

impl Present {
    /// The device bytes the render pass reads: the 16-byte extent and the frame's own depth buffer. Printed by
    /// any caller that wants to state what a windowed frame costs on the bus — which is nothing, since both
    /// numbers are resident before the frame and stay there through it.
    pub fn bound_bytes(&self) -> u64 {
        self.extent.size() + self.depth_bytes
    }
}

/// Where the camera is, in the form `interp.wgsl`'s `CameraUniform` wants.
///
/// The basis vectors arrive pre-built rather than as an eye/target/up triple because `render::raster::Camera`
/// already derives them, and a second derivation in the shader would be a second place for the two to
/// disagree. Every field here is presentation: `ray_for`'s output is what the host path uploads today, so
/// nothing a root depends on changes hands (AGENTS.md §4, SPEC-02).
#[derive(Debug, Clone)]
pub struct GpuCamera {
    pub origin: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    /// Half the viewport height in world units at the near plane — `render::raster`'s `half_height`.
    pub half_height: f32,
    pub max_t: f32,
    pub surface: f32,
    pub max_steps: u32,
    pub root: u32,
    pub width: u32,
    pub height: u32,
}

/// One distance comparison.
pub struct Agreement {
    pub adapter: String,
    pub samples: usize,
    /// `max |gpu - cpu|` in world units, with the point that produced it.
    pub max_abs_deviation: f32,
    pub worst_point: Vec3,
    pub gpu_at_worst: f32,
    pub cpu_at_worst: f32,
    /// Points where either side returned the depth/unsupported sentinel; those agree trivially and must
    /// not be counted as evidence, so they are reported separately.
    pub sentinels: usize,
}

fn bytes_of_u32(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

fn bytes_of_f32(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Four little-endian `f32`s as a stack array, for the paths that may not allocate per value.
fn vec4_bytes(values: [f32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&values[0].to_le_bytes());
    out[4..8].copy_from_slice(&values[1].to_le_bytes());
    out[8..12].copy_from_slice(&values[2].to_le_bytes());
    out[12..16].copy_from_slice(&values[3].to_le_bytes());
    out
}

/// A shape tree as four `u32` words per node, in `contracts::sdf::NodeRecord`'s order - the bytes an
/// `assetc` run writes, with no repacking between the file and the buffer.
pub fn node_bytes(tree: &Tree) -> Vec<u8> {
    let words: Vec<u32> = tree.nodes.iter().flat_map(|node| node.0).collect();
    bytes_of_u32(&words)
}

impl Interpreter {
    /// Open a device, preferring the fallback (software) adapter so the check runs on the same
    /// architecture in CI and on a laptop, and only then trying the machine's own GPU.
    /// Open the machine's own GPU and nothing else, for the measurements where the software adapter
    /// would report a different quantity than the one being asked about.
    ///
    /// [`Interpreter::open`] deliberately tries the fallback adapter first, because the agreement check wants
    /// the same result on every CI architecture and on a laptop. A *timing* run wants the opposite: the
    /// Microsoft Basic Render Driver is a CPU rasteriser, and queueing work to it measures submission cost and
    /// software maths, not a frame on the device gate R1 names. So the two entry points differ on purpose
    /// rather than one of them being wrong.
    pub fn open_hardware() -> Result<Interpreter, OpenError> {
        pollster::block_on(Self::open_with(false))
    }

    pub fn open() -> Result<Interpreter, OpenError> {
        for fallback in [true, false] {
            match pollster::block_on(Self::open_with(fallback)) {
                Ok(interpreter) => return Ok(interpreter),
                Err(OpenError::NoAdapter) => continue,
                Err(OpenError::NoDevice) => return Err(OpenError::NoDevice),
            }
        }
        Err(OpenError::NoAdapter)
    }

    /// Open an interpreter with a specific adapter, bypassing the automatic selection.
    /// This allows benchmarks and tests to force a particular GPU instead of relying on wgpu's ranking.
    pub fn open_with_adapter(adapter: wgpu::Adapter) -> Result<Interpreter, OpenError> {
        let info = adapter.get_info();
        let adapter_name = format!("{} [{:?}, {:?}]", info.name, info.backend, info.device_type);
        let instance = wgpu::Instance::default(); // Dummy instance, not used after device creation

        pollster::block_on(Self::from_adapter(adapter, instance, adapter_name))
    }

    async fn from_adapter(
        adapter: wgpu::Adapter,
        instance: wgpu::Instance,
        adapter_name: String,
    ) -> Result<Interpreter, OpenError> {
        // Temporal surface cache needs 8 storage buffers (4 core + 4 cache) across 2 bind groups.
        // Downlevel defaults only allow 4 storage buffers total, so we request desktop-class limits.
        // This still works on integrated GPUs and most mobile GPUs - the MX250 supports it fine.
        let mut limits = wgpu::Limits::downlevel_defaults();
        limits.max_storage_buffers_per_shader_stage = 8;
        limits.max_bind_groups = 2;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("a07-agreement"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|_| OpenError::NoDevice)?;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("a07-master-interpreter"),
            source: wgpu::ShaderSource::Wgsl(include_str!("interp.wgsl").into()),
        });
        // `layout: None` lets wgpu derive the bind group layout from the shader itself, which is the
        // point of this check: if the WGSL's bindings and the CPU side's uploads disagree, the failure
        // surfaces as a validation error here rather than as a shader quietly reading the wrong buffer.
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("evaluate_points"),
            layout: None,
            module: &module,
            entry_point: Some("evaluate_points"),
            compilation_options: Default::default(),
            cache: None,
        });
        let march_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("march_rays"),
            layout: None,
            module: &module,
            entry_point: Some("march_rays"),
            compilation_options: Default::default(),
            cache: None,
        });
        // Derived from the entry point, like the other two, so the bindings this pipeline requires are
        // exactly the ones `march_rays_from_camera` reads: no ray buffer at all, plus the camera block.
        // If the shader and this file disagree, validation says so at open() rather than as a wrong picture.
        let camera_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("march_rays_from_camera"),
            layout: None,
            module: &module,
            entry_point: Some("march_rays_from_camera"),
            compilation_options: Default::default(),
            cache: None,
        });
        // The counted march: one pipeline over the same module, whose entry point differs only in writing
        // its depth at a doubled stride so the arm's per-ray tally has somewhere to go. Derived layout as
        // above, so a binding the counted entry point stops reading would show up here as a validation
        // error rather than as a tally that silently counts nothing.
        let counted_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("march_rays_counted"),
            layout: None,
            module: &module,
            entry_point: Some("march_rays_counted"),
            compilation_options: Default::default(),
            cache: None,
        });

        Ok(Interpreter {
            device,
            queue,
            instance,
            window_adapter: adapter,
            pipeline,
            march_pipeline,
            camera_pipeline,
            counted_pipeline,
            adapter: adapter_name,
        })
    }

    async fn open_with(fallback: bool) -> Result<Interpreter, OpenError> {
        let instance = wgpu::Instance::default();
        // Chosen by device_type, not by request_adapter's power preference: with HighPerformance this still
        // returned the Microsoft Basic Render Driver (a Cpu adapter) on a machine that enumerated a discrete
        // GPU and an iGPU, so a "GPU" agreement or frame curve measured that way would be software
        // rasterisation reported as hardware. Discrete first, then integrated, then anything that is not a
        // CPU device; the fallback path asks for the CPU device on purpose and labels it.
        let mut adapters: Vec<wgpu::Adapter> = instance.enumerate_adapters(wgpu::Backends::all());
        if !fallback {
            // Rank alone is not enough: on a runner whose only device is the software rasteriser, sorting
            // Cpu last still returns it, and a timing number from it would be quoted as a GPU measurement.
            // Asking for hardware means rejecting the non-hardware adapters, so the caller gets NoAdapter
            // and prints a skip instead.
            adapters.retain(|a| a.get_info().device_type != wgpu::DeviceType::Cpu);
        }
        adapters.sort_by_key(|a| {
            let t = a.get_info().device_type;
            if fallback {
                if t == wgpu::DeviceType::Cpu {
                    0
                } else {
                    1
                }
            } else {
                match t {
                    wgpu::DeviceType::DiscreteGpu => 0,
                    wgpu::DeviceType::IntegratedGpu => 1,
                    wgpu::DeviceType::Cpu => 3,
                    _ => 2,
                }
            }
        });
        let adapter = adapters.into_iter().next().ok_or(OpenError::NoAdapter)?;
        let info = adapter.get_info();
        let adapter_name = format!("{} [{:?}, {:?}]", info.name, info.backend, info.device_type);
        // Temporal surface cache needs 8 storage buffers (4 core + 4 cache) across 2 bind groups.
        // Downlevel defaults only allow 4 storage buffers total, so we request desktop-class limits.
        // This still works on integrated GPUs and most mobile GPUs - the MX250 supports it fine.
        let mut limits = wgpu::Limits::downlevel_defaults();
        limits.max_storage_buffers_per_shader_stage = 8;
        limits.max_bind_groups = 2;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("a07-agreement"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|_| OpenError::NoDevice)?;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("a07-master-interpreter"),
            source: wgpu::ShaderSource::Wgsl(include_str!("interp.wgsl").into()),
        });
        // `layout: None` lets wgpu derive the bind group layout from the shader itself, which is the
        // point of this check: if the WGSL's bindings and the CPU side's uploads disagree, the failure
        // surfaces as a validation error here rather than as a shader quietly reading the wrong buffer.
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("evaluate_points"),
            layout: None,
            module: &module,
            entry_point: Some("evaluate_points"),
            compilation_options: Default::default(),
            cache: None,
        });
        let march_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("march_rays"),
            layout: None,
            module: &module,
            entry_point: Some("march_rays"),
            compilation_options: Default::default(),
            cache: None,
        });
        // Derived from the entry point, like the other two, so the bindings this pipeline requires are
        // exactly the ones `march_rays_from_camera` reads: no ray buffer at all, plus the camera block.
        // If the shader and this file disagree, validation says so at open() rather than as a wrong picture.
        let camera_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("march_rays_from_camera"),
            layout: None,
            module: &module,
            entry_point: Some("march_rays_from_camera"),
            compilation_options: Default::default(),
            cache: None,
        });
        // The counted march: one pipeline over the same module, whose entry point differs only in writing
        // its depth at a doubled stride so the arm's per-ray tally has somewhere to go. Derived layout as
        // above, so a binding the counted entry point stops reading would show up here as a validation
        // error rather than as a tally that silently counts nothing.
        let counted_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("march_rays_counted"),
            layout: None,
            module: &module,
            entry_point: Some("march_rays_counted"),
            compilation_options: Default::default(),
            cache: None,
        });

        Ok(Interpreter {
            device,
            queue,
            instance,
            window_adapter: adapter,
            pipeline,
            march_pipeline,
            camera_pipeline,
            counted_pipeline,
            adapter: adapter_name,
        })
    }

    /// Which adapter this ran on, so a deviation can be attributed to a vendor rather than to
    /// "the CI".
    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    /// Return a reference to the wgpu queue for buffer writes (e.g., dirty mask invalidation).
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Return a reference to the wgpu device for polling and synchronization.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Evaluate `tree` at `points` on the device and compare with the reference.
    pub fn compare_with_reference(
        &self,
        tree: &Tree,
        points: &[Vec3],
    ) -> Result<Agreement, String> {
        if points.is_empty() {
            return Err("no points to compare".to_string());
        }
        let storage = wgpu::BufferUsages::STORAGE;
        let nodes = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("nodes"),
                contents: &node_bytes(tree),
                usage: storage,
            });
        let pool = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pool"),
                contents: &bytes_of_f32(&tree.params),
                usage: storage,
            });
        // One vec4 per query: the point, and the root index as a float the shader reads back as u32.
        let mut query_bytes = Vec::with_capacity(points.len() * 16);
        for point in points {
            query_bytes.extend_from_slice(&bytes_of_f32(point));
            query_bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        let queries = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("queries"),
                contents: &query_bytes,
                usage: storage,
            });
        let header_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("header"),
                contents: &bytes_of_u32(&u32_le_header(points.len(), STACK_DEPTH)),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let size = (points.len() * 4) as u64;
        let distances = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("distances"),
            size,
            usage: storage | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // A storage buffer cannot be mapped - WebGPU forbids it - so the results are copied into a
        // staging buffer whose only job is to be read back. Two buffers, not one convenience flag.
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("distances-staging"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, &nodes),
                entry(1, &pool),
                entry(2, &queries),
                entry(3, &header_buf),
                entry(4, &distances),
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(points.len().div_ceil(64) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&distances, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);

        let raw = self.read(&staging, size)?;
        let mut worst = Agreement {
            adapter: self.adapter.clone(),
            samples: points.len(),
            max_abs_deviation: 0.0,
            worst_point: points[0],
            gpu_at_worst: 0.0,
            cpu_at_worst: 0.0,
            sentinels: 0,
        };
        for (index, point) in points.iter().enumerate() {
            let gpu = f32::from_le_bytes(
                raw[index * 4..index * 4 + 4]
                    .try_into()
                    .map_err(|_| "short read")?,
            );
            let cpu = sdf_eval(tree, ROOT, *point);
            if gpu > UNSUPPORTED_HALF || cpu > UNSUPPORTED_HALF {
                worst.sentinels += 1;
                continue;
            }
            let deviation = (gpu - cpu).abs();
            if deviation > worst.max_abs_deviation {
                worst.max_abs_deviation = deviation;
                worst.worst_point = *point;
                worst.gpu_at_worst = gpu;
                worst.cpu_at_worst = cpu;
            }
        }
        Ok(worst)
    }

    /// Time repeated dispatches of the master interpreter over one fixed grid.
    ///
    /// Two numbers on purpose, because they measure different things and only one of them resembles a frame:
    /// `ms_submit_only` is what a thread pays to queue the work, `ms_per_pass_with_read` is end-to-end until
    /// the distances are back in CPU memory, which is what anything that consumes them waits on. Reported per
    /// pass and per evaluation, never as a verdict against gate R1: R1 is a *march* with culling and a BVH on
    /// a mid-range GPU, and this is one field evaluation per pixel on a tree of a few nodes.
    pub fn bench(
        &self,
        tree: &Tree,
        points: &[Vec3],
        dispatches: usize,
        read_dispatches: usize,
    ) -> Result<Bench, String> {
        if points.is_empty() || dispatches == 0 {
            return Err("nothing to bench".to_string());
        }
        let storage = wgpu::BufferUsages::STORAGE;
        let nodes = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bench-nodes"),
                contents: &node_bytes(tree),
                usage: storage,
            });
        let pool = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bench-pool"),
                contents: &bytes_of_f32(&tree.params),
                usage: storage,
            });
        let mut query_bytes = Vec::with_capacity(points.len() * 16);
        for point in points {
            query_bytes.extend_from_slice(&bytes_of_f32(point));
            query_bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        let queries = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bench-queries"),
                contents: &query_bytes,
                usage: storage,
            });
        let header_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bench-header"),
                contents: &bytes_of_u32(&u32_le_header(points.len(), STACK_DEPTH)),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let size = (points.len() * 4) as u64;
        let distances = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench-distances"),
            size,
            usage: storage | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench-staging"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, &nodes),
                entry(1, &pool),
                entry(2, &queries),
                entry(3, &header_buf),
                entry(4, &distances),
            ],
        });
        let workgroups = points.len().div_ceil(64) as u32;
        let submit = |me: &Self| {
            let mut encoder = me
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                pass.set_pipeline(&me.pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups(workgroups, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&distances, 0, &staging, 0, size);
            me.queue.submit([encoder.finish()]);
        };

        // The closure borrows `bind`/`staging`, so the clock is started and stopped around plain loops
        // rather than inside a helper that would have to own them.
        let start = std::time::Instant::now();
        for _ in 0..dispatches {
            submit(self);
        }
        let submit_only = start.elapsed().as_secs_f64() * 1000.0;
        // Drain the queue before the second mode, or the read-back timings would absorb the work the
        // first mode queued and both numbers would describe the same interval.
        let _ = self.read(&staging, size)?;

        let start = std::time::Instant::now();
        let read_dispatches = read_dispatches.min(dispatches).max(1);
        for _ in 0..read_dispatches {
            submit(self);
            self.read(&staging, size)?;
        }
        let with_read = start.elapsed().as_secs_f64() * 1000.0 / read_dispatches as f64;

        // Third column: N dispatches queued, then one retrieval - a frame's field work with the grid copied
        // back once, which is the shape a device-resident march has. Measured rather than multiplied.
        let start = std::time::Instant::now();
        for _ in 0..dispatches {
            submit(self);
        }
        self.read(&staging, size)?;
        let batch_with_single_read = start.elapsed().as_secs_f64() * 1000.0;

        Ok(Bench {
            adapter: self.adapter.clone(),
            points: points.len(),
            dispatches,
            evaluations: points.len() * dispatches,
            ms_submit_only_total: submit_only,
            ms_per_pass_with_read: with_read,
            ms_batch_with_single_read: batch_with_single_read,
        })
    }

    /// March `rays` against one subtree of `tree` on the device and return the depths.
    ///
    /// One upload, one dispatch, one copy back: the shape a frame has, as opposed to `bench`'s shape of
    /// N uploads of a whole grid. The step law itself is `interp.wgsl`'s `march_rays`, transcribed from
    /// `render`'s `march_with`; this function reports what the device produced and does not certify that
    /// the two agree - the example that does that is the measurement's correctness anchor.
    pub fn march(&self, tree: &Tree, root: u32, rays: &[Ray]) -> Result<March, String> {
        self.march_uploaded(&self.upload(tree), root, rays)
    }

    /// Put a shape on the device once, so a later frame can bind it without allocating or copying its
    /// bytes again. The `.len()` fields are taken before the buffers move.
    pub fn upload(&self, tree: &Tree) -> Shape {
        let nodes = node_bytes(tree);
        let pool = pool_bytes(tree);
        self.shape_from(nodes, pool, tree.nodes.len())
    }

    /// The same resident shape with every bound row **unlicensed**: the tail is still four words per node, so
    /// the shader's `arrayLength(&pool) - 4 * arrayLength(&nodes)` arithmetic lands on a radius word, but
    /// every radius is `+inf` and `length(p - centre) - inf >= left` is never true. It is therefore a control
    /// the arm cannot fire on while the pool layout, the node records and the walk stay byte-for-byte what
    /// `upload` made — the only difference between the two shapes is what the bound test answers.
    ///
    /// This exists so the device's bound-skip saving can be *measured* rather than bounded: docs/04 quotes the
    /// CPU cull layer's avoided entries as an upper bound on the arm, and an upper bound is not a number on the
    /// thing. Durations are still informational here (ADR-017) — what makes the pair admissible is that the two
    /// marches' depths are asserted identical, so the cheaper one did not compute a different picture.
    pub fn upload_bounds_refused(&self, tree: &Tree) -> Shape {
        let nodes = node_bytes(tree);
        let mut pool = bytes_of_f32(&tree.params);
        for _ in 0..tree.nodes.len() {
            pool.extend_from_slice(&vec4_bytes([0.0, 0.0, 0.0, f32::INFINITY]));
        }
        self.shape_from(nodes, pool, tree.nodes.len())
    }

    /// The same resident shape with every bound **smaller by `delta` world units**, which is the falsifier a
    /// ray-level arm needs: `r1_cull_price` counts 1,104 rays that a bare radius refuses where a bound grown by
    /// the march's own tolerance must march them, and 921,232 more once every enclosure lies by a full unit. An
    /// arm that cannot lose surfaces when its bounds lie is not reading the bounds, so the count of lost
    /// surfaces is the assertion that makes the identical-count row above mean something.
    ///
    /// Only the radius moves. The node records, the pool layout and the four-words-per-node tail stay
    /// byte-identical to `upload`'s, and an unlicensed row (`+inf`) is unaffected by the subtraction, so the
    /// rows the CPU refuses to license still answer "reachable" here.
    pub fn upload_bounds_shrunk(&self, tree: &Tree, delta: f32) -> Shape {
        let nodes = node_bytes(tree);
        let mut pool = bytes_of_f32(&tree.params);
        for row in render::cull::scene_bounds(tree).encoded() {
            pool.extend_from_slice(&vec4_bytes([row[0], row[1], row[2], row[3] - delta]));
        }
        self.shape_from(nodes, pool, tree.nodes.len())
    }

    /// Create the two storage buffers from already-encoded bytes. `.len()` fields are read before the buffers
    /// move, so the capacities a later `upload_into` checks against are this shape's own.
    fn shape_from(&self, nodes: Vec<u8>, pool: Vec<u8>, node_count: usize) -> Shape {
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        Shape {
            bytes: nodes.len() + pool.len(),
            node_count,
            nodes_capacity: nodes.len() as u64,
            pool_capacity: pool.len() as u64,
            nodes: self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("shape-nodes"),
                    contents: &nodes,
                    usage: storage,
                }),
            pool: self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("shape-pool"),
                    contents: &pool,
                    usage: storage,
                }),
        }
    }

    /// Move a changed shape onto the device **without creating a buffer**: the two `write_buffer` calls land in
    /// the bytes `upload` already allocated. This is AS4's asset-side clause measured instead of asserted — an
    /// edit that grows a tree past the capacity it first uploaded refuses here rather than silently re-creating,
    /// because a re-creation is the allocation churn §3 bans. Returns the bytes written.
    pub fn upload_into(&self, shape: &mut Shape, tree: &Tree) -> Result<usize, String> {
        let nodes = node_bytes(tree);
        let pool = pool_bytes(tree);
        if nodes.len() as u64 > shape.nodes_capacity || pool.len() as u64 > shape.pool_capacity {
            return Err(format!(
                "a {}-node / {}-param-byte shape does not fit the {} / {} bytes already resident - re-uploading \
                 into a reused asset buffer grows nothing, so a tree that outgrows its first upload needs a new \
                 upload (and the allocation that clause refuses)",
                nodes.len() / 16,
                pool.len(),
                shape.nodes_capacity,
                shape.pool_capacity
            ));
        }
        self.queue.write_buffer(&shape.nodes, 0, &nodes);
        self.queue.write_buffer(&shape.pool, 0, &pool);
        shape.bytes = nodes.len() + pool.len();
        shape.node_count = tree.nodes.len();
        Ok(shape.bytes)
    }

    /// Everything a march does with a shape that is already resident — the half a reused frame pays.
    pub fn march_uploaded(&self, shape: &Shape, root: u32, rays: &[Ray]) -> Result<March, String> {
        self.march_with(&self.march_pipeline, shape, root, rays, 1)
            .map(|(march, _)| march)
    }

    /// The same dispatch as `march_uploaded`, plus the device's own per-ray tally of the union second
    /// children its bound arm refused to evaluate. Two words come back per ray, so the buffer and the
    /// copy-back are twice the size — which is why this is an instrument and not the frame path:
    /// `march_frame` keeps its one-word-per-ray layout, and with it AS4's buffer-reuse clause.
    pub fn march_counted(&self, shape: &Shape, root: u32, rays: &[Ray]) -> Result<Counted, String> {
        self.march_with(&self.counted_pipeline, shape, root, rays, 2)
            .map(|(march, skips)| Counted { march, skips })
    }

    /// One march dispatch, for either output layout: `words_per_ray` is 1 for `march_rays` and 2 for
    /// `march_rays_counted`, whose extra word per ray is the arm's tally. The plumbing is shared on
    /// purpose — a second copy of dispatch-and-read-back is the fork this file has avoided since
    /// `march_one` was extracted so the camera path could reuse the law.
    fn march_with(
        &self,
        pipeline: &wgpu::ComputePipeline,
        shape: &Shape,
        root: u32,
        rays: &[Ray],
        words_per_ray: usize,
    ) -> Result<(March, Vec<u32>), String> {
        let Some(first) = rays.first() else {
            return Err("no rays to march".to_string());
        };
        // The step cap and the surface tolerance are one header pair for the whole dispatch, so a grid
        // that mixed them would trace some rays under a different law than the number being reported.
        for ray in rays {
            if ray.surface != first.surface || ray.max_steps != first.max_steps {
                return Err("rays disagree on surface or max_steps".to_string());
            }
        }
        let storage = wgpu::BufferUsages::STORAGE;
        let nodes = &shape.nodes;
        let pool = &shape.pool;
        // Two vec4s per ray, read out of the buffer the point-query path already binds: a stage may not
        // have six storage buffers on the downlevel limits this device was opened with.
        //
        // The bytes go straight into one pre-sized buffer. Repacking each vec4 through a `Vec` allocates
        // twice per ray - 6,144 tiny allocations for a 64x48 frame, which is the malloc/free churn
        // AGENTS.md §3 bans on a gameplay frame - and the buffer's length is fixed by the ray count, so a
        // growable allocation is buying nothing here.
        let mut query_bytes = vec![0u8; rays.len() * 32];
        for (index, ray) in rays.iter().enumerate() {
            let start = index * 32;
            let chunk = &mut query_bytes[start..start + 32];
            chunk[..16].copy_from_slice(&vec4_bytes([
                ray.origin[0],
                ray.origin[1],
                ray.origin[2],
                root as f32,
            ]));
            chunk[16..]
                .copy_from_slice(&vec4_bytes([ray.dir[0], ray.dir[1], ray.dir[2], ray.max_t]));
        }
        let queries = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("march-rays"),
                contents: &query_bytes,
                usage: storage,
            });
        let header_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("march-header"),
                contents: &bytes_of_u32(&[
                    rays.len() as u32,
                    STACK_DEPTH,
                    first.max_steps,
                    first.surface.to_bits(),
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let size = (rays.len() * 4 * words_per_ray) as u64;
        let traced = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("march-depths"),
            size,
            usage: storage | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("march-staging"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, nodes),
                entry(1, pool),
                entry(2, &queries),
                entry(3, &header_buf),
                entry(4, &traced),
            ],
        });
        let workgroups = rays.len().div_ceil(64) as u32;

        let start = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&traced, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let ms_submit_only = start.elapsed().as_secs_f64() * 1000.0;
        let raw = self.read(&staging, size)?;
        let ms_frame_with_read = start.elapsed().as_secs_f64() * 1000.0;

        // Depth words and tally words arrive interleaved when the entry point appends one per ray, so a
        // lane index decides where each word goes. The tallies come back as the integers they were written
        // as — `bitcast<f32>` in, `to_bits` out — never compared through a float.
        let mut depths = Vec::with_capacity(rays.len());
        let mut skips = Vec::with_capacity(if words_per_ray > 1 { rays.len() } else { 0 });
        let mut offset = 0usize;
        let mut lane = 0usize;
        while offset + 4 <= raw.len() {
            let word = f32::from_le_bytes(
                raw[offset..offset + 4]
                    .try_into()
                    .map_err(|_| "short read")?,
            );
            if lane == 0 {
                depths.push(word);
            } else {
                skips.push(word.to_bits());
            }
            lane = (lane + 1) % words_per_ray;
            offset += 4;
        }
        let hits = depths
            .iter()
            .filter(|depth| **depth < UNSUPPORTED_HALF)
            .count();
        Ok((
            March {
                adapter: self.adapter.clone(),
                rays: rays.len(),
                max_steps: first.max_steps,
                surface: first.surface,
                hits,
                ms_submit_only,
                ms_frame_with_read,
                depths,
            },
            skips,
        ))
    }

    /// The buffers a frame owns once, for a fixed ray count: packed rays in, depths out, plus the header
    /// pair the shader reads.
    ///
    /// `march_uploaded` still creates four buffers and a bind group per call, which is what AS4's
    /// no-allocation-churn clause is about — and what lost this repo a device at 1080p, where a fresh 66 MB
    /// ray buffer per march makes the driver thrash. A `Frame` is allocated once and then refilled by
    /// `write_buffer`, so a repeated frame touches no new device memory and its only host allocation is the
    /// depth vector it hands back.
    pub fn begin_frame(&self, ray_count: usize) -> Frame {
        let storage = wgpu::BufferUsages::STORAGE;
        let depth_bytes = (ray_count * 4) as u64;
        Frame {
            rays: ray_count,
            queries: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame-rays"),
                size: (ray_count * 32) as u64,
                usage: storage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            header: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame-header"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            traced: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame-depths"),
                size: depth_bytes,
                usage: storage | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            staging: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame-staging"),
                size: depth_bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            packed: vec![0u8; ray_count * 32],
            law: None,
        }
    }

    /// Bind a resident shape to a frame's buffers: the pair a running renderer holds between frames, made
    /// once per (shape, frame) rather than once per frame.
    pub fn bind(&self, shape: &Shape, frame: &Frame) -> Bind {
        Bind {
            bind: self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.march_pipeline.get_bind_group_layout(0),
                entries: &[
                    entry(0, &shape.nodes),
                    entry(1, &shape.pool),
                    entry(2, &frame.queries),
                    entry(3, &frame.header),
                    entry(4, &frame.traced),
                ],
            }),
            cache_bind: None, // Regular frames don't use temporal cache
            dirty_mask_buf: None,
            dirty_shadow: RefCell::new(Vec::new()),
            cache_stats_buf: None,
            width: 0,
            height: 0,
        }
    }

    /// Fill a frame whose buffers already exist: pack the rays into the byte buffer `begin_frame` allocated
    /// once, then move them and the header with two `write_buffer` calls. Nothing here creates a buffer, a bind
    /// group or a pipeline.
    ///
    /// Split from `march_frame` so the two directions of the host round trip can be priced apart — this call is
    /// the host-to-device copy, that one is the dispatch plus the read-back — and it returns the byte count it
    /// wrote, so a caller divides by a measured figure instead of hoping `rays * 32` is the width on the bus.
    pub fn write_rays(&self, frame: &mut Frame, root: u32, rays: &[Ray]) -> Result<usize, String> {
        let Some(first) = rays.first() else {
            return Err("no rays to write".to_string());
        };
        if rays.len() != frame.rays {
            return Err(format!(
                "this frame was built for {} rays and the grid has {}",
                frame.rays,
                rays.len()
            ));
        }
        for ray in rays {
            if ray.surface != first.surface || ray.max_steps != first.max_steps {
                return Err("rays disagree on surface or max_steps".to_string());
            }
        }
        for (index, ray) in rays.iter().enumerate() {
            let start = index * 32;
            let chunk = &mut frame.packed[start..start + 32];
            chunk[..16].copy_from_slice(&vec4_bytes([
                ray.origin[0],
                ray.origin[1],
                ray.origin[2],
                root as f32,
            ]));
            chunk[16..]
                .copy_from_slice(&vec4_bytes([ray.dir[0], ray.dir[1], ray.dir[2], ray.max_t]));
        }
        let mut header = [0u8; 16];
        for (offset, word) in [
            rays.len() as u32,
            STACK_DEPTH,
            first.max_steps,
            first.surface.to_bits(),
        ]
        .iter()
        .enumerate()
        {
            header[offset * 4..offset * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        self.queue.write_buffer(&frame.queries, 0, &frame.packed);
        self.queue.write_buffer(&frame.header, 0, &header);
        frame.law = Some((first.max_steps, first.surface));
        Ok(frame.packed.len())
    }

    /// March the frame `write_rays` last filled. The cap and tolerance reported are the ones that call recorded
    /// on the frame, so this cannot state a law the device was never handed; a frame nobody has written into is
    /// an error, not a zero-filled march.
    pub fn march_frame(&self, bind: &Bind, frame: &Frame) -> Result<March, String> {
        let Some((max_steps, surface)) = frame.law else {
            return Err("march_frame on a frame no rays have been written into".to_string());
        };
        let size = (frame.rays * 4) as u64;
        let start = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.march_pipeline);
            pass.set_bind_group(0, &bind.bind, &[]);
            pass.dispatch_workgroups(frame.rays.div_ceil(64) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&frame.traced, 0, &frame.staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let ms_submit_only = start.elapsed().as_secs_f64() * 1000.0;
        let raw = self.read(&frame.staging, size)?;
        let ms_frame_with_read = start.elapsed().as_secs_f64() * 1000.0;

        let mut depths = Vec::with_capacity(frame.rays);
        let mut offset = 0usize;
        while offset + 4 <= raw.len() {
            depths.push(f32::from_le_bytes(
                raw[offset..offset + 4]
                    .try_into()
                    .map_err(|_| "short read")?,
            ));
            offset += 4;
        }
        let hits = depths
            .iter()
            .filter(|depth| **depth < UNSUPPORTED_HALF)
            .count();
        Ok(March {
            adapter: self.adapter.clone(),
            rays: frame.rays,
            max_steps,
            surface,
            hits,
            ms_submit_only,
            ms_frame_with_read,
            depths,
        })
    }

    /// The buffers a device-generated frame owns once: a header, a camera block, the depths out. There is
    /// deliberately **no ray buffer**, which is the whole point of the path. At 1920x1080 the host-written
    /// frame is 66,355,200 bytes of rays down the bus per frame (`begin_frame` allocates `rays * 32` and
    /// `write_rays` uploads all of it); this one moves 16 + 96 bytes in and the depths back.
    ///
    /// Like `begin_frame`, the allocation is once per run rather than once per frame, so a repeated frame
    /// still touches no new device memory (AS4's clause).
    pub fn begin_camera_frame(&self, width: u32, height: u32) -> CameraFrame {
        let pixels = width as usize * height as usize;
        let depth_bytes = (pixels * 4) as u64;
        CameraFrame {
            pixels,
            width,
            height,
            header: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("camera-header"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            camera: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("camera-uniform"),
                size: 96,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            traced: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("camera-depths"),
                size: depth_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            staging: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("camera-staging"),
                size: depth_bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            law: None,
        }
    }

    /// Put the header and the camera block on the device, and record the law the frame now carries.
    ///
    /// The layout is `interp.wgsl`'s `CameraUniform`, word for word: four 16-byte rows of direction and
    /// position (the fourth word of the first row is the root index), then geometry as
    /// `(half_height, aspect, max_t, unused)` and the grid as `(width, height, unused, unused)`. Aspect is
    /// computed here rather than passed in, because the shader derives `ndc_x` from it exactly the way
    /// `render::raster::ray_for` does, and a caller-supplied aspect is a second place for it to be wrong.
    ///
    /// Returns the byte count it wrote to the bus, which is the number the host-ray path is compared on.
    pub fn write_camera(&self, frame: &mut CameraFrame, camera: &GpuCamera) -> usize {
        let aspect = camera.width as f32 / camera.height as f32;
        let mut header = [0u8; 16];
        for (offset, word) in [
            frame.pixels as u32,
            STACK_DEPTH,
            camera.max_steps,
            camera.surface.to_bits(),
        ]
        .iter()
        .enumerate()
        {
            header[offset * 4..offset * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let mut block = [0u8; 96];
        let rows: [(usize, Vec3); 4] = [
            (0, camera.origin),
            (16, camera.right),
            (32, camera.up),
            (48, camera.forward),
        ];
        for (offset, vector) in rows {
            for (axis, word) in vector.iter().enumerate() {
                block[offset + axis * 4..offset + axis * 4 + 4]
                    .copy_from_slice(&word.to_le_bytes());
            }
        }
        block[12..16].copy_from_slice(&camera.root.to_le_bytes());
        for (index, word) in [camera.half_height, aspect, camera.max_t, 0.0f32]
            .iter()
            .enumerate()
        {
            block[64 + index * 4..68 + index * 4].copy_from_slice(&word.to_le_bytes());
        }
        for (index, word) in [camera.width as f32, camera.height as f32, 0.0f32, 64.0f32]
            .iter()
            .enumerate()
        {
            block[80 + index * 4..84 + index * 4].copy_from_slice(&word.to_le_bytes());
        }
        self.queue.write_buffer(&frame.header, 0, &header);
        self.queue.write_buffer(&frame.camera, 0, &block);
        frame.law = Some((camera.max_steps, camera.surface));
        header.len() + block.len()
    }

    /// Bind a resident shape to a camera frame. Slot 2 - the ray buffer - is absent, because the entry
    /// point this pipeline was derived from never names it.
    pub fn bind_camera(&self, shape: &Shape, frame: &CameraFrame) -> Bind {
        // Unculled path: no temporal cache, only bind group 0
        Bind {
            bind: self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("camera-march"),
                layout: &self.camera_pipeline.get_bind_group_layout(0),
                entries: &[
                    entry(0, &shape.nodes),
                    entry(1, &shape.pool),
                    entry(3, &frame.header),
                    entry(4, &frame.traced),
                    entry(5, &frame.camera),
                ],
            }),
            cache_bind: None,
            dirty_mask_buf: None,
            dirty_shadow: RefCell::new(Vec::new()),
            cache_stats_buf: None,
            width: frame.width,
            height: frame.height,
        }
    }

    /// Bind a frame to a caller-chosen camera pipeline. The layout comes from the pipeline itself, exactly as
    /// `bind_camera` derives it, so a variant that stopped reading one of these buffers fails validation rather
    /// than quietly measuring a frame that never used the scene.
    pub fn bind_camera_variant(
        &self,
        variant: &CameraVariant,
        shape: &Shape,
        frame: &CameraFrame,
    ) -> Bind {
        self.bind_camera_on(&variant.pipeline, variant.temporal_cache, shape, frame)
    }

    fn bind_camera_on(
        &self,
        pipeline: &wgpu::ComputePipeline,
        temporal_cache: bool,
        shape: &Shape,
        frame: &CameraFrame,
    ) -> Bind {
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-march"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, &shape.nodes),
                entry(1, &shape.pool),
                entry(3, &frame.header),
                entry(4, &frame.traced),
                entry(5, &frame.camera),
            ],
        });
        if !temporal_cache {
            // The march this pipeline was compiled for never names a `@group(1)` buffer, so the layout wgpu
            // derived for it stops at group 0 - which is also what `bind_camera` hands back for the shipped
            // camera pipeline. Asking for the group anyway was a validation error on every GPU, and only the
            // runners that actually get an adapter can reach it.
            return Bind {
                bind,
                cache_bind: None,
                dirty_mask_buf: None,
                dirty_shadow: RefCell::new(Vec::new()),
                cache_stats_buf: None,
                width: frame.width,
                height: frame.height,
            };
        }

        // Create temporal cache buffers for bind group 1
        let pixels = frame.pixels;
        let pixel_size = core::mem::size_of::<f32>();

        // cached_positions: vec3<f32> per pixel (12 bytes each, padded to 16)
        let positions_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-cache-positions"),
            size: (pixels * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // cached_depths: f32 per pixel
        let depths_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-cache-depths"),
            size: (pixels * pixel_size) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // cached_normals: vec3<f32> per pixel (12 bytes each, padded to 16)
        let normals_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-cache-normals"),
            size: (pixels * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // cached_metadata: vec4<u32> per pixel (16 bytes each)
        let metadata_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-cache-metadata"),
            size: (pixels * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // dirty_mask: 1 bit per pixel for delta rendering (u32 words, 32 pixels per word)
        let dirty_words = pixels.div_ceil(32);
        let dirty_mask_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-dirty-mask"),
            size: (dirty_words * 4) as u64, // 4 bytes per u32
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        // cache_stats: [cache_hits, total_dirty_checked, reserved, reserved]
        let cache_stats_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-cache-stats"),
            size: 16, // 4 x u32
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Initialize all buffers to zero/invalid state
        let zero_bytes_pos = vec![0u8; pixels * 16];
        self.queue.write_buffer(&positions_buf, 0, &zero_bytes_pos);

        let zero_bytes_depth = vec![0u8; pixels * pixel_size];
        self.queue.write_buffer(&depths_buf, 0, &zero_bytes_depth);

        let zero_bytes_norm = vec![0u8; pixels * 16];
        self.queue.write_buffer(&normals_buf, 0, &zero_bytes_norm);

        let zero_bytes_meta = vec![0u8; pixels * 16];
        self.queue.write_buffer(&metadata_buf, 0, &zero_bytes_meta);

        // Initialize dirty mask: all bits set (all pixels dirty on first frame)
        let all_dirty: Vec<u8> = vec![0xFF; dirty_words * 4];
        self.queue.write_buffer(&dirty_mask_buf, 0, &all_dirty);

        // Initialize cache stats to zero
        let zero_stats: Vec<u8> = vec![0u8; 16];
        self.queue.write_buffer(&cache_stats_buf, 0, &zero_stats);

        // Create bind group 1 using the pipeline's auto-derived layout
        // This should work now that we've increased device limits to support 8 storage buffers
        let cache_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal-cache"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[
                entry(0, &positions_buf),
                entry(1, &depths_buf),
                entry(2, &normals_buf),
                entry(3, &metadata_buf),
                entry(4, &dirty_mask_buf),
                entry(5, &cache_stats_buf),
            ],
        });

        Bind {
            bind,
            cache_bind: Some(cache_bind),
            dirty_mask_buf: Some(dirty_mask_buf),
            dirty_shadow: RefCell::new(vec![0xFFFF_FFFF; dirty_words]),
            cache_stats_buf: Some(cache_stats_buf),
            width: frame.width,
            height: frame.height,
        }
    }
}

/// Mark one pixel rectangle dirty in a host-side copy of the dirty mask, and return the run of mask words it
/// touched.
///
/// The rectangle is frame-clamped, so an out-of-bounds or empty region marks nothing and returns `None`. The mask
/// is built at `u64` width because a run can end exactly on a word boundary and ask for `1 << 32`; the arithmetic
/// this replaces branched around that case, and the branch is what left it deriving the last word from the
/// exclusive end and indexing an inverted slice range.
fn mark_region(
    shadow: &mut [u32],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Option<Range<usize>> {
    if shadow.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let x_start = x.min(width);
    let y_start = y.min(height);
    let x_end = x.saturating_add(w).min(width);
    let y_end = y.saturating_add(h).min(height);
    if x_start >= x_end || y_start >= y_end {
        return None;
    }

    let mut first_word = usize::MAX;
    let mut last_word = 0usize;
    for row in y_start..y_end {
        let pixel_start = row * width + x_start;
        let pixel_end = row * width + x_end;
        let row_first = (pixel_start / 32) as usize;
        // `pixel_end` is exclusive, so the last word the region occupies is one before its boundary when the
        // boundary lands on a word edge. Deriving it from `pixel_end - 1` makes that case fall out of the
        // division instead of needing a branch — the branch is what inverted the range here.
        let row_last = ((pixel_end - 1) / 32) as usize;
        for (offset, slot) in shadow[row_first..=row_last].iter_mut().enumerate() {
            let base = ((row_first + offset) as u32) * 32;
            let lo = pixel_start.max(base) - base;
            let hi = pixel_end.min(base + 32) - base;
            *slot |= ((1u64 << hi) - (1u64 << lo)) as u32;
        }
        first_word = first_word.min(row_first);
        last_word = last_word.max(row_last);
    }
    Some(first_word..last_word + 1)
}

/// Copy a run of host mask words to the device at the byte offset those words live at.
fn write_words(queue: &wgpu::Queue, mask: &wgpu::Buffer, shadow: &[u32], span: Range<usize>) {
    let mut bytes = Vec::with_capacity(span.len() * 4);
    for &word in &shadow[span.clone()] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    queue.write_buffer(mask, (span.start * 4) as u64, &bytes);
}

impl Bind {
    /// Invalidate a rectangular region of the dirty mask, forcing those pixels to be re-marched.
    ///
    /// This simulates entity movement by marking pixels within a bounding box as dirty. The region is
    /// specified in pixel coordinates (x, y, width, height) and clamped to the frame; nothing outside it is
    /// touched, which is the entire point of the delta path — the mask words that span crosses are the only
    /// bytes written, and at 1080p the mask as a whole is 259,200 of them.
    ///
    /// # Arguments
    /// * `queue` - The wgpu queue to submit the write to
    /// * `x`, `y` - Top-left corner of the region in pixels
    /// * `w`, `h` - Width and height of the region in pixels
    pub fn invalidate_region(&self, queue: &wgpu::Queue, x: u32, y: u32, w: u32, h: u32) {
        let Some(ref dirty_mask_buf) = self.dirty_mask_buf else {
            return; // No dirty mask buffer, nothing to invalidate
        };

        let mut shadow = self.dirty_shadow.borrow_mut();
        let Some(span) = mark_region(&mut shadow, self.width, self.height, x, y, w, h) else {
            return;
        };
        write_words(queue, dirty_mask_buf, &shadow, span);
    }

    /// Invalidate all pixels (force full re-march on next frame)
    pub fn invalidate_all(&self, queue: &wgpu::Queue) {
        self.write_full_mask(queue, 0xFFFF_FFFF);
    }

    /// Clear all dirty bits (mark all pixels as clean/cached).
    /// Useful for resetting between test runs.
    pub fn clear_dirty_mask(&self, queue: &wgpu::Queue) {
        self.write_full_mask(queue, 0);
    }

    /// Set every mask word to one value on the device and in the host mirror, together.
    fn write_full_mask(&self, queue: &wgpu::Queue, word: u32) {
        let Some(ref dirty_mask_buf) = self.dirty_mask_buf else {
            return;
        };
        let mut shadow = self.dirty_shadow.borrow_mut();
        shadow.fill(word);
        let len = shadow.len();
        write_words(queue, dirty_mask_buf, &shadow, 0..len);
    }

    /// Empty the host mirror after submitting a frame that marched the whole grid through the cache.
    ///
    /// `march_rays_from_camera_culled` clears a pixel's dirty bit on every path out of it - hit, sky, and
    /// marched - and its only early return is for `index >= header.point_count`, which a camera frame sets to
    /// its own pixel count, so a submitted frame leaves the device mask all zeros. The mirror has to agree or
    /// the dirt of every frame since the last invalidation accumulates in it and gets re-pushed one region at a
    /// time: the delta path would be marching the union of all past regions while believing it was caching. A
    /// frame that marched only part of the grid would make this call wrong, and would need the mirror emptied
    /// by the pixels that were read rather than by the whole frame.
    fn take_consumed_mask(&self) {
        if self.cache_bind.is_some() {
            self.dirty_shadow.borrow_mut().fill(0);
        }
    }

    /// Read back the dirty mask from GPU memory.
    /// Returns a vector of u32 words representing the dirty state, or None if there is no temporal
    /// cache or the read-back did not complete.
    pub fn read_dirty_mask(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Vec<u32>> {
        let dirty_mask_buf = self.dirty_mask_buf.as_ref()?;

        let total_words = (self.width * self.height).div_ceil(32) as usize;
        let byte_size = (total_words * 4) as u64;

        // Create staging buffer
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dirty-mask-staging"),
            size: byte_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Copy from GPU to staging
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dirty-mask-copy"),
        });
        encoder.copy_buffer_to_buffer(dirty_mask_buf, 0, &staging, 0, byte_size);
        queue.submit(Some(encoder.finish()));

        // Read back
        let slice = staging.slice(0..byte_size);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        // A failed poll means the staging buffer never mapped, and reading it would panic; the
        // caller's `None` is the honest answer for "no mask could be read".
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;

        let view = slice.get_mapped_range();
        let words: Vec<u32> = view
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_le_bytes(*chunk))
            .collect();
        drop(view);
        staging.unmap();

        // The device is the authority on which bits survive a march, so what came back becomes the mirror
        // rather than being compared with it. `write_buffer` cannot OR, so every later partial write is built
        // from this copy and has to match the device word for word outside the span it touches.
        *self.dirty_shadow.borrow_mut() = words.clone();
        Some(words)
    }

    /// Read cache statistics after a frame render.
    /// Returns (cache_hits, total_dirty_pixels_checked) or None if no temporal cache is active or
    /// the read-back did not complete.
    pub fn read_cache_stats(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<(u32, u32)> {
        let cache_stats_buf = self.cache_stats_buf.as_ref()?;

        // Create a staging buffer to copy into
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cache-stats-staging"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Copy from GPU to staging
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cache-stats-copy"),
        });
        encoder.copy_buffer_to_buffer(cache_stats_buf, 0, &staging, 0, 16);
        queue.submit(Some(encoder.finish()));

        // Read back
        let slice = staging.slice(0..16);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;

        let view = slice.get_mapped_range();
        let stats: [u32; 4] = [
            u32::from_le_bytes(view[0..4].try_into().unwrap()),
            u32::from_le_bytes(view[4..8].try_into().unwrap()),
            u32::from_le_bytes(view[8..12].try_into().unwrap()),
            u32::from_le_bytes(view[12..16].try_into().unwrap()),
        ];
        drop(view);
        staging.unmap();

        Some((stats[0], stats[1]))
    }

    /// Invalidate multiple rectangular regions in a single GPU write.
    ///
    /// More efficient than calling invalidate_region() multiple times when you have
    /// several entities to invalidate, as it batches all writes into one transfer: the words between
    /// distant regions are rewritten from the mirror unchanged, so one copy covers them all.
    ///
    /// # Arguments
    /// * `queue` - The wgpu queue to submit the write to
    /// * `regions` - Slice of (x, y, width, height) tuples for each region
    pub fn invalidate_regions(&self, queue: &wgpu::Queue, regions: &[(u32, u32, u32, u32)]) {
        let Some(ref dirty_mask_buf) = self.dirty_mask_buf else {
            return;
        };

        let mut shadow = self.dirty_shadow.borrow_mut();
        let mut span: Option<Range<usize>> = None;
        for &(x, y, w, h) in regions {
            let Some(touched) = mark_region(&mut shadow, self.width, self.height, x, y, w, h)
            else {
                continue;
            };
            span = Some(match span {
                Some(existing) => existing.start.min(touched.start)..existing.end.max(touched.end),
                None => touched,
            });
        }

        if let Some(span) = span {
            write_words(queue, dirty_mask_buf, &shadow, span);
        }
    }

    /// Invalidate an entity's screen-space bounding box.
    ///
    /// Projects a world-space axis-aligned bounding box (AABB) to screen space using the camera
    /// parameters, then invalidates the resulting pixel region. This is more convenient than
    /// manually computing screen coordinates when tracking moving entities.
    ///
    /// # Arguments
    /// * `queue` - The wgpu queue to submit the write to
    /// * `camera` - Camera parameters for projection
    /// * `world_min` - Minimum corner of entity's world-space AABB [x, y, z]
    /// * `world_max` - Maximum corner of entity's world-space AABB [x, y, z]
    ///
    /// # Returns
    /// Invalidate all pixels covered by a world-space AABB.
    ///
    /// Projects the 8 corners of the AABB to screen space and marks the bounding
    /// rectangle as dirty. Note: this only projects corner points, so the resulting
    /// screen bounds may be larger than the actual projected entity for rotated or
    /// non-axis-aligned objects (conservative over-approximation).
    ///
    /// Returns `Some((x, y, w, h))` if any pixels were marked dirty, `None` if culled.
    pub fn invalidate_entity(
        &self,
        queue: &wgpu::Queue,
        camera: &GpuCamera,
        world_min: [f32; 3],
        world_max: [f32; 3],
    ) -> Option<(u32, u32, u32, u32)> {
        // Project world-space AABB corners to clip space
        let project_point = |p: [f32; 3]| -> Option<[f32; 2]> {
            // Transform to camera space
            let rel = [
                p[0] - camera.origin[0],
                p[1] - camera.origin[1],
                p[2] - camera.origin[2],
            ];

            // Check if point is behind camera or too close for stable projection
            let forward_dot = rel[0] * camera.forward[0]
                + rel[1] * camera.forward[1]
                + rel[2] * camera.forward[2];
            if forward_dot <= 0.001 {
                return None; // Behind camera or too close (prevents division by near-zero)
            }

            // Project to screen space
            let right_dot =
                rel[0] * camera.right[0] + rel[1] * camera.right[1] + rel[2] * camera.right[2];
            let up_dot = rel[0] * camera.up[0] + rel[1] * camera.up[1] + rel[2] * camera.up[2];

            // Guard against numerical instability when half_height is very small
            let denom = forward_dot * camera.half_height;
            if denom.abs() < 1e-6 {
                return None; // Denominator too small, projection would be unstable
            }

            let ndc_x = right_dot / denom;
            let ndc_y = up_dot / denom;

            // Convert to pixel coordinates
            let px = ((ndc_x + 1.0) * 0.5) * camera.width as f32;
            let py = ((1.0 - ndc_y) * 0.5) * camera.height as f32;

            Some([px, py])
        };

        // Project all 8 corners of the AABB and find screen-space bounds
        let mut min_px = f32::MAX;
        let mut max_px = f32::MIN;
        let mut min_py = f32::MAX;
        let mut max_py = f32::MIN;
        let mut any_visible = false;

        for x in &[world_min[0], world_max[0]] {
            for y in &[world_min[1], world_max[1]] {
                for z in &[world_min[2], world_max[2]] {
                    if let Some([px, py]) = project_point([*x, *y, *z]) {
                        min_px = min_px.min(px);
                        max_px = max_px.max(px);
                        min_py = min_py.min(py);
                        max_py = max_py.max(py);
                        any_visible = true;
                    }
                }
            }
        }

        if !any_visible {
            return None; // Entity completely behind camera
        }

        // Clamp to viewport and convert to integer pixel coordinates
        let x_start = min_px.max(0.0).min(camera.width as f32) as u32;
        let y_start = min_py.max(0.0).min(camera.height as f32) as u32;
        let x_end = max_px.max(0.0).min(camera.width as f32) as u32;
        let y_end = max_py.max(0.0).min(camera.height as f32) as u32;

        if x_start >= x_end || y_start >= y_end {
            return None; // Degenerate rectangle
        }

        let w = x_end - x_start;
        let h = y_end - y_start;

        self.invalidate_region(queue, x_start, y_start, w, h);
        Some((x_start, y_start, w, h))
    }
}

impl Interpreter {
    /// Whether the camera pipeline compiled from `source` for entry point `entry` has a `@group(1)` to bind.
    ///
    /// Asked rather than assumed, because `layout: None` means the layout is whatever the entry point's own
    /// resources are: the unculled march has one group and the culled one two, and binding a cache group the
    /// pipeline never derived is a device-side validation error that no adapter-less CI runner can see.
    fn camera_has_cache_group(source: &str, entry: &str) -> Result<bool, String> {
        Ok(crate::naga_validate::temporal_cache_owner(source)? == Some(entry))
    }

    /// Compile the shipped interpreter with its `DEPTH` capacity replaced, for the cost ladder. Refuses rather
    /// than silently handing back the shipped module: the pattern has to be present exactly once, because a
    /// ladder whose rows are all the same pipeline would print a stack price of zero and look like a finding.
    pub fn camera_variant(&self, depth: u32) -> Result<CameraVariant, String> {
        const DEPTH_LINE: &str = "const DEPTH: u32 = 32u;";
        const ENTRY: &str = "march_rays_from_camera";
        let shipped = crate::naga_validate::INTERP;
        if shipped.matches(DEPTH_LINE).count() != 1 {
            return Err(format!(
                "interp.wgsl's `{DEPTH_LINE}` line is not found exactly once, so the ladder cannot attribute a \
                 row to a capacity"
            ));
        }
        let source = shipped.replace(DEPTH_LINE, &format!("const DEPTH: u32 = {depth}u;"));
        let temporal_cache = Self::camera_has_cache_group(&source, ENTRY)?;
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &module,
                entry_point: Some(ENTRY),
                compilation_options: Default::default(),
                cache: None,
            });
        Ok(CameraVariant {
            depth,
            pipeline,
            temporal_cache,
        })
    }

    /// Compile the shipped interpreter with its entry point moved to the ray-culled camera march, for task #69's
    /// arm. Nothing else in the module changes - same `const DEPTH`, same `march_one`, same pool tail the
    /// bound-skip arm already reads - so the two pipelines are one grid apart in a name, which is what lets an
    /// example compare their depths bit for bit and call the difference the arm's.
    ///
    /// Refuses rather than handing back the shipped pipeline: a module whose culled entry point has been renamed
    /// or deleted would otherwise measure an unculled frame and report it as the armed one, the same failure the
    /// `DEPTH_LINE` check above exists to make impossible.
    pub fn camera_cull_variant(&self) -> Result<CameraVariant, String> {
        const ENTRY: &str = "march_rays_from_camera_culled";
        let shipped = crate::naga_validate::INTERP;
        if shipped.matches(&format!("fn {ENTRY}(")).count() != 1 {
            return Err(format!(
                "interp.wgsl's culled camera entry point `{ENTRY}` is not found exactly once, so the arm has no pipeline to name - a row measured here would be the unculled frame wearing the cull's label"
            ));
        }
        let temporal_cache = Self::camera_has_cache_group(shipped, ENTRY)?;
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("interp-culled-camera"),
                source: wgpu::ShaderSource::Wgsl(shipped.into()),
            });

        // Use auto-generated layout from shader (wgpu infers bind groups from @group/@binding annotations)
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &module,
                entry_point: Some(ENTRY),
                compilation_options: Default::default(),
                cache: None,
            });

        Ok(CameraVariant {
            depth: STACK_DEPTH,
            pipeline,
            temporal_cache,
        })
    }

    /// March a device-generated frame: one dispatch of width x height rays, then the depths back. The law
    /// reported is the one `write_camera` recorded, so this cannot state a law the device never received.
    pub fn march_camera_frame(&self, bind: &Bind, frame: &CameraFrame) -> Result<March, String> {
        self.march_camera_on(&self.camera_pipeline, bind, frame)
    }

    /// The same dispatch and read-back through a caller-chosen camera pipeline, so a ladder's rows differ only
    /// in the constant their modules were compiled with.
    pub fn march_camera_variant(
        &self,
        variant: &CameraVariant,
        bind: &Bind,
        frame: &CameraFrame,
    ) -> Result<March, String> {
        self.march_camera_on(&variant.pipeline, bind, frame)
    }

    fn march_camera_on(
        &self,
        pipeline: &wgpu::ComputePipeline,
        bind: &Bind,
        frame: &CameraFrame,
    ) -> Result<March, String> {
        let Some((max_steps, surface)) = frame.law else {
            return Err(
                "march_camera_frame on a frame no camera has been written into".to_string(),
            );
        };
        let size = (frame.pixels * 4) as u64;
        let start = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind.bind, &[]);
            // Set temporal cache bind group if present
            if let Some(ref cache_bind) = bind.cache_bind {
                pass.set_bind_group(1, cache_bind, &[]);
            }
            pass.dispatch_workgroups(frame.pixels.div_ceil(64) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&frame.traced, 0, &frame.staging, 0, size);
        self.queue.submit([encoder.finish()]);
        bind.take_consumed_mask();
        let ms_submit_only = start.elapsed().as_secs_f64() * 1000.0;
        let raw = self.read(&frame.staging, size)?;
        let ms_frame_with_read = start.elapsed().as_secs_f64() * 1000.0;

        let mut depths = Vec::with_capacity(frame.pixels);
        let mut offset = 0usize;
        while offset + 4 <= raw.len() {
            depths.push(f32::from_le_bytes(
                raw[offset..offset + 4]
                    .try_into()
                    .map_err(|_| "short read")?,
            ));
            offset += 4;
        }
        let hits = depths
            .iter()
            .filter(|depth| **depth < UNSUPPORTED_HALF)
            .count();
        Ok(March {
            adapter: self.adapter.clone(),
            rays: frame.pixels,
            max_steps,
            surface,
            hits,
            ms_submit_only,
            ms_frame_with_read,
            depths,
        })
    }

    /// The march with nothing coming back: encode, submit, and wait for the queue to drain without copying or
    /// mapping a single depth.
    ///
    /// Split from `march_camera_frame` because the two are the last two terms of an R1 frame once the ray bytes
    /// are gone — the device's own stepping and the `pixels * 4` read-back — and only an on-device shading pass
    /// deletes the second. Returns the milliseconds the caller waited, which is an attribution attempt rather
    /// than a frame time: if a driver's wait returns before the compute queue is truly drained, the read-back
    /// leg absorbs the difference and the split is wrong in the direction its printed line names. What the split
    /// is *not* taken on trust from is the march's completion — `read_camera_depths` below asserts on the bytes
    /// this call produced, so a march that had not run would show up there as the wrong hit count.
    pub fn march_camera_frame_no_readback(
        &self,
        bind: &Bind,
        frame: &CameraFrame,
    ) -> Result<f64, String> {
        self.march_camera_no_readback_on(&self.camera_pipeline, bind, frame)
    }

    /// The no-read-back march through a caller-chosen camera pipeline. This is the leg the cost ladder times:
    /// a variant's `DEPTH` changes only the stack each invocation carries, so the step work and the byte count
    /// are the same, and the difference between two rows is the difference in their per-invocation state.
    pub fn march_camera_variant_no_readback(
        &self,
        variant: &CameraVariant,
        bind: &Bind,
        frame: &CameraFrame,
    ) -> Result<f64, String> {
        self.march_camera_no_readback_on(&variant.pipeline, bind, frame)
    }

    fn march_camera_no_readback_on(
        &self,
        pipeline: &wgpu::ComputePipeline,
        bind: &Bind,
        frame: &CameraFrame,
    ) -> Result<f64, String> {
        if frame.law.is_none() {
            return Err(
                "march_camera_frame_no_readback on a frame no camera has been written into"
                    .to_string(),
            );
        }
        let start = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind.bind, &[]);
            // Set temporal cache bind group if present
            if let Some(ref cache_bind) = bind.cache_bind {
                pass.set_bind_group(1, cache_bind, &[]);
            }
            pass.dispatch_workgroups(frame.pixels.div_ceil(64) as u32, 1, 1);
        }
        self.queue.submit([encoder.finish()]);
        bind.take_consumed_mask();
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("poll: {error}"))?;
        Ok(start.elapsed().as_secs_f64() * 1000.0)
    }

    /// The surface a window draws into, taken from the instance this device was opened against. Generic over
    /// anything that can produce raw window handles rather than over a windowing crate, so `winit` stays a
    /// dev-dependency and the boot's headless published output cannot acquire a display requirement.
    pub fn open_surface<'w, W>(&self, window: W) -> Result<wgpu::Surface<'w>, String>
    where
        W: raw_window_handle::HasWindowHandle
            + raw_window_handle::HasDisplayHandle
            + Send
            + Sync
            + 'w,
    {
        self.instance
            .create_surface(window)
            .map_err(|error| format!("create_surface: {error}"))
    }

    /// Agree a drawable size with the window, configure the surface at it, and return that configuration.
    ///
    /// The size is not advisory: a `CameraFrame` is allocated for a fixed pixel count, so a window that is
    /// resized needs a new frame, and the caller that builds one is the caller that has to know what the
    /// surface now expects. Zero is refused rather than marched: a 0-pixel frame is a dispatch that does
    /// nothing and a clear that paints nothing, which would read as a black window rather than as an error.
    pub fn configure_surface(
        &self,
        surface: &wgpu::Surface<'_>,
        width: u32,
        height: u32,
    ) -> Result<wgpu::SurfaceConfiguration, String> {
        if width == 0 || height == 0 {
            return Err(format!(
                "a {width}x{height} drawable has no pixels to march"
            ));
        }
        let config = surface
            .get_default_config(&self.window_adapter, width, height)
            .ok_or_else(|| "no surface configuration this adapter accepts".to_string())?;
        surface.configure(&self.device, &config);
        Ok(config)
    }

    /// Attach a window (or any surface texture) to a marched frame: one render pipeline, one bind group and
    /// one 16-byte extent block, created here and reused by every frame after the first.
    ///
    /// The bind group points at the frame's own `traced` buffer, so presenting costs no copy and no read-back
    /// — the depths an R1 harness maps to the host stay on the device and are sampled straight from storage.
    /// The pipeline's layout is derived from `present.wgsl` itself, exactly as the camera pipelines are: if
    /// the shader stops reading the depth buffer, this call fails validation instead of painting a window of
    /// sky. The target format has to be the surface's, which is why it is an argument and not a constant.
    pub fn begin_presentation(
        &self,
        frame: &CameraFrame,
        target: wgpu::TextureFormat,
        width: u32,
        height: u32,
        max_t: f32,
    ) -> Present {
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("present-depth-ramp"),
                source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("present-depth-ramp"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_full_screen"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_depth_ramp"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
        // `max_t` is in the ramp because a distance without the scene's scale in it is arbitrary: 12 world
        // units is a wall or the horizon depending on how far the camera can see.
        let mut block = [0u8; 16];
        for (offset, word) in [width, height, max_t.to_bits(), 0u32].iter().enumerate() {
            block[offset * 4..offset * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let extent = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("present-extent"),
                contents: &block,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[entry(0, &extent), entry(1, &frame.traced)],
        });
        Present {
            device: self.device.clone(),
            queue: self.queue.clone(),
            depth_bytes: frame.traced.size(),
            pipeline,
            bind,
            extent,
        }
    }

    /// Copy and map the depths of a frame that has already been marched, dispatching nothing. This is the
    /// read-back term on its own, so a caller can ask whether the bytes or the stepping is what a frame costs.
    /// The byte count is taken from the copy this call makes, not from an arithmetic figure the caller writes.
    pub fn read_camera_depths(&self, frame: &CameraFrame) -> Result<ReadBack, String> {
        if frame.law.is_none() {
            return Err(
                "read_camera_depths on a frame no camera has been written into".to_string(),
            );
        }
        let size = (frame.pixels * 4) as u64;
        let start = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&frame.traced, 0, &frame.staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let raw = self.read(&frame.staging, size)?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut depths = Vec::with_capacity(frame.pixels);
        let mut offset = 0usize;
        while offset + 4 <= raw.len() {
            depths.push(f32::from_le_bytes(
                raw[offset..offset + 4]
                    .try_into()
                    .map_err(|_| "short read")?,
            ));
            offset += 4;
        }
        let hits = depths
            .iter()
            .filter(|depth| **depth < UNSUPPORTED_HALF)
            .count();
        Ok(ReadBack {
            bytes: raw.len(),
            hits,
            depths,
            ms,
        })
    }

    fn read(&self, buffer: &wgpu::Buffer, size: u64) -> Result<Vec<u8>, String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, 0..size, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("poll: {error}"))?;
        let status = receiver
            .recv()
            .map_err(|_| "the map callback never ran".to_string())?;
        status.map_err(|error| format!("map_async: {error}"))?;
        let out = buffer.slice(0..size).get_mapped_range().to_vec();
        buffer.unmap();
        Ok(out)
    }
}

impl Present {
    /// One frame on the window: encode the pass, submit, and let the surface present. Nothing is uploaded,
    /// copied or mapped here — the depths the shader samples are the ones the march already put on the device.
    ///
    /// The submit is not waited on: a windowed frame that blocked on its own queue would be a benchmark with a
    /// display attached, not a frame. Callers that need the march's numbers wait on `march_camera_frame`, which
    /// does poll.
    pub fn draw(&self, view: &wgpu::TextureView) {
        self.draw_cleared(
            view,
            wgpu::Color {
                r: 0.02,
                g: 0.03,
                b: 0.05,
                a: 1.0,
            },
        );
    }

    /// The same pass with a chosen background. `read_drawable` uses a colour `present.wgsl` cannot emit so
    /// that a pixel the fragment stage never visited stays distinguishable from a pixel it decided is sky.
    fn draw_cleared(&self, view: &wgpu::TextureView, clear: wgpu::Color) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("present"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("depth ramp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Draw this pass into a texture the call owns and hand the host the bytes it ended up with.
    ///
    /// This exists because of one gap `demo_window`'s verification cannot close. That example compares the
    /// *depth buffer* ray for ray against `render::march`, which pins the march, the camera law and the
    /// buffer's own row order — but the fragment stage chooses which drawable row a buffer row lands on, and
    /// a flip there is invisible to a check that never reads the drawable. It would show an upside-down
    /// world, and every assertion on trunk would still pass. The witness for that class of mistake was a
    /// screenshot; this makes it a number.
    ///
    /// The format is `Rgba8Unorm` rather than the surface's because this is a measurement path, not a
    /// present path: a surface texture is not necessarily copyable, and an 8-bit linear target stores the
    /// shader's own fractions byte for byte, so the host can predict them.
    ///
    /// The one thing this pass does differ in is its clear colour, and that difference is the instrument:
    /// magenta `(255, 0, 255)` is a triple `present.wgsl` cannot produce (its sky is `(5, 8, 13)` and its ramp
    /// never reaches 255 red or 0 green), so a pixel still holding it is a pixel the fragment stage was never
    /// run for. Clearing to the demo's own sky colour, as the window path does, would make an
    /// under-covered frame and an all-sky frame the same bytes.
    pub fn read_drawable(&self, width: u32, height: u32) -> Result<(Vec<u8>, u32), String> {
        let bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = (bytes_per_row * height) as u64;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("present-witness"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw_cleared(
            &view,
            wgpu::Color {
                r: 1.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
        );
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("present-witness-staging"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("present-witness-copy"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        // The same map-and-wait `Interpreter::read` performs; it is spelled out here because `Present` owns a
        // device and a queue, not an interpreter, and this path is the render stage's, not the march's.
        let (sender, receiver) = std::sync::mpsc::channel();
        staging.map_async(wgpu::MapMode::Read, 0..size, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("poll: {error}"))?;
        let status = receiver
            .recv()
            .map_err(|_| "the map callback never ran".to_string())?;
        status.map_err(|error| format!("map_async: {error}"))?;
        let raw = staging.slice(0..size).get_mapped_range().to_vec();
        staging.unmap();
        Ok((raw, bytes_per_row))
    }
}

const UNSUPPORTED_HALF: f32 = 1.0e37;

fn u32_le_header(point_count: usize, stack_depth: u32) -> Vec<u32> {
    vec![point_count as u32, stack_depth, 0, 0]
}

fn entry<'a>(index: u32, buffer: &'a wgpu::Buffer) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding: index,
        resource: buffer.as_entire_binding(),
    }
}

/// A deterministic sample grid: no RNG and no clock, so a deviation reported by one machine describes the
/// same points another machine would test.
pub fn sample_grid() -> Vec<Vec3> {
    let mut out = Vec::new();
    for x in -6..=6 {
        for y in -4..=4 {
            for z in -6..=6 {
                out.push([x as f32 * 0.42, y as f32 * 0.37, z as f32 * 0.45]);
            }
        }
    }
    out
}

#[cfg(test)]
mod dirty_mask_tests {
    use super::*;

    /// The mask a set of rectangles should produce, built one pixel at a time with no word arithmetic at all.
    ///
    /// `mark_region`'s job is to set the same bits this naive loop does while only touching the words that
    /// change, so the reference is deliberately the slow, obvious kind: a bug shared between the two would
    /// have to be a bug in the pixel rectangle itself.
    fn naive(width: u32, height: u32, regions: &[(u32, u32, u32, u32)]) -> Vec<u32> {
        let mut words = vec![0u32; (width * height).div_ceil(32) as usize];
        for &(x, y, w, h) in regions {
            for py in y..(y + h).min(height) {
                for px in x..(x + w).min(width) {
                    let pixel = py * width + px;
                    words[(pixel / 32) as usize] |= 1 << (pixel % 32);
                }
            }
        }
        words
    }

    /// The smallest run of words covering every non-zero word in `words`.
    fn run(words: &[u32]) -> Range<usize> {
        match (
            words.iter().position(|&word| word != 0),
            words.iter().rposition(|&word| word != 0),
        ) {
            (Some(first), Some(last)) => first..last + 1,
            _ => 0..0,
        }
    }

    fn set_pixels(words: &[u32]) -> u32 {
        words.iter().map(|word| word.count_ones()).sum()
    }

    #[test]
    fn every_rectangle_in_a_small_frame_marks_exactly_its_pixels() {
        for width in [7u32, 32, 33, 64] {
            let height = 4;
            for y in 0..height {
                for x in 0..width {
                    for h in 1..=(height - y) {
                        for w in 1..=(width - x) {
                            let mut shadow = vec![0u32; (width * height).div_ceil(32) as usize];
                            let expected = naive(width, height, &[(x, y, w, h)]);
                            let touched = mark_region(&mut shadow, width, height, x, y, w, h)
                                .expect("a rectangle inside the frame marks something");
                            assert_eq!(shadow, expected, "width {width} region {x},{y} {w}x{h}");
                            assert_eq!(touched, run(&expected), "span for {x},{y} {w}x{h}");
                        }
                    }
                }
            }
        }
    }

    /// The shape that panicked: a row segment inside one 32-pixel word, aligned at neither end.
    #[test]
    fn a_region_that_fits_inside_one_word_marks_its_own_bits() {
        let (width, height) = (64u32, 4u32);
        let mut shadow = vec![0u32; (width * height).div_ceil(32) as usize];
        // Row 1, x 5..13 is pixels 69..77: one word, neither end on a boundary.
        let touched = mark_region(&mut shadow, width, height, 5, 1, 8, 1).unwrap();
        assert_eq!(touched, 2..3);
        assert_eq!(
            shadow,
            vec![0, 0, 0x1FE0, 0, 0, 0, 0, 0],
            "pixels 69..=76 are bits 5..=12 of word 2"
        );
    }

    /// The delta path's whole purpose, stated as an assertion: after a frame has emptied the mask, invalidating
    /// one entity's box dirties that box and nothing else.
    #[test]
    fn one_region_does_not_dirty_the_frame_around_it() {
        let (width, height) = (128u32, 64u32);
        let mut shadow = vec![0u32; (width * height).div_ceil(32) as usize];
        mark_region(&mut shadow, width, height, 40, 20, 16, 16).unwrap();
        assert_eq!(set_pixels(&shadow), 16 * 16);
        assert_eq!(shadow, naive(width, height, &[(40, 20, 16, 16)]));
    }

    /// A run that ends exactly on a word edge asks for `1 << 32`, which is the shift that overflowed when the
    /// mask was built at `u32` width.
    #[test]
    fn a_run_ending_on_a_word_boundary_keeps_its_shift() {
        let mut shadow = vec![0u32; 8];
        mark_region(&mut shadow, 64, 1, 20, 0, 12, 1).unwrap();
        assert_eq!(shadow[0], 0xFFF0_0000);
        assert_eq!(shadow[1], 0);
    }

    #[test]
    fn a_region_outside_the_frame_marks_nothing() {
        let (width, height) = (64u32, 4u32);
        let mut shadow = vec![0u32; (width * height).div_ceil(32) as usize];
        for region in [(70, 0, 8, 1), (0, 4, 8, 8), (0, 0, 0, 4), (0, 0, 4, 0)] {
            assert_eq!(
                mark_region(
                    &mut shadow,
                    width,
                    height,
                    region.0,
                    region.1,
                    region.2,
                    region.3
                ),
                None,
                "{region:?} is not a region this frame has pixels for"
            );
        }
        assert!(shadow.iter().all(|&word| word == 0));
    }

    /// A region wider than the frame clamps instead of wrapping: the pixels it claims are the ones inside.
    #[test]
    fn a_region_bigger_than_the_frame_is_clamped_not_wrapped() {
        let mut shadow = vec![0u32; 2];
        let touched = mark_region(&mut shadow, 32, 2, 8, 0, u32::MAX, u32::MAX).unwrap();
        assert_eq!(shadow, naive(32, 2, &[(8, 0, 24, 2)]));
        assert_eq!(touched, 0..2);
    }

    /// One batch write has to reach every word any of its regions changed, which is the union of their runs.
    #[test]
    fn batching_regions_covers_every_word_the_separate_calls_do() {
        let (width, height) = (100u32, 30u32);
        let words = (width * height).div_ceil(32) as usize;
        let regions = [(3, 2, 9, 7), (33, 5, 40, 4), (70, 21, 25, 6)];

        let mut batched = vec![0u32; words];
        let mut batch_span: Option<Range<usize>> = None;
        let mut separate = vec![0u32; words];
        for &(x, y, w, h) in &regions {
            let touched = mark_region(&mut batched, width, height, x, y, w, h).unwrap();
            batch_span = Some(match batch_span {
                Some(existing) => existing.start.min(touched.start)..existing.end.max(touched.end),
                None => touched,
            });
            mark_region(&mut separate, width, height, x, y, w, h).unwrap();
        }
        assert_eq!(batched, separate);
        assert_eq!(batch_span.unwrap(), run(&separate));
        assert_eq!(set_pixels(&batched), 9 * 7 + 40 * 4 + 25 * 6);
    }

    /// A fresh bind hands the mask over as all-dirty, because the first frame has no cache to reuse: the
    /// mirror starts matching the device, or the first invalidation would clear dirt the device still holds.
    #[test]
    fn a_dirty_mirror_stays_dirty_when_a_region_is_added_to_it() {
        let (width, height) = (64u32, 4u32);
        let mut shadow = vec![0xFFFF_FFFFu32; (width * height).div_ceil(32) as usize];
        mark_region(&mut shadow, width, height, 5, 1, 8, 1).unwrap();
        assert!(shadow.iter().all(|&word| word == 0xFFFF_FFFF));
    }
}
