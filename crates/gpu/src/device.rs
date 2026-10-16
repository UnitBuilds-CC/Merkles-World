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
    pipeline: wgpu::ComputePipeline,
    march_pipeline: wgpu::ComputePipeline,
    adapter: String,
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("a07-agreement"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
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

        Ok(Interpreter {
            device,
            queue,
            pipeline,
            march_pipeline,
            adapter: adapter_name,
        })
    }

    /// Which adapter this ran on, so a deviation can be attributed to a vendor rather than to
    /// "the CI".
    pub fn adapter(&self) -> &str {
        &self.adapter
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
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let nodes = node_bytes(tree);
        let pool = bytes_of_f32(&tree.params);
        Shape {
            bytes: nodes.len() + pool.len(),
            node_count: tree.nodes.len(),
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
        let pool = bytes_of_f32(&tree.params);
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
        let size = (rays.len() * 4) as u64;
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
            layout: &self.march_pipeline.get_bind_group_layout(0),
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
            pass.set_pipeline(&self.march_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&traced, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let ms_submit_only = start.elapsed().as_secs_f64() * 1000.0;
        let raw = self.read(&staging, size)?;
        let ms_frame_with_read = start.elapsed().as_secs_f64() * 1000.0;

        let mut depths = Vec::with_capacity(rays.len());
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
            rays: rays.len(),
            max_steps: first.max_steps,
            surface: first.surface,
            hits,
            ms_submit_only,
            ms_frame_with_read,
            depths,
        })
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
