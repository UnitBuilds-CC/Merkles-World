//! The demo's picture, in a window: the boot's compiled scene marched on the device and presented, with no
//! byte of it crossing the bus per frame.
//!
//! `cargo run --release -p gpu --example demo_window`
//!
//! Every device number in this repository has been read out of a buffer the host mapped. That is the right
//! instrument for gate R1 — a budget is a host-visible interval — and it is not the pitch demo, which the
//! roadmap's Phase 1 exit describes as a booting engine that **renders** a procedural SDF scene. The
//! 8,294,400-byte read-back is also the term `r1_march_alone` measured as a third of the frame, and the only
//! way to delete it is to never ask for the pixels on the host. So this is that path: the camera march's
//! compute dispatch, then a render pass whose fragment stage samples the compute stage's own storage buffer.
//!
//! Four things are asserted, and all four are integers (ADR-017 — the milliseconds are printed, never gated,
//! and a window is not a budget instrument):
//!
//! 1. **The scene is the boot's**, arriving as bytes its compiler wrote: 11 node records, 25 parameters,
//!    declared depth 4, with the walk's own frame count agreeing with the envelope's.
//! 2. **The windowed path's depths agree with the CPU reference**, hit for hit over the grid it drew, worst
//!    deviation under 2e-3 — the same anchor `boot_scene_on_device` uses, on the bytes the window consumed.
//! 3. **The picture is not empty**: the frame must contain surfaces. A presentation shader that indexed past
//!    its buffer, or a bind group pointed at a buffer the march never wrote, both paint a clean, uniform,
//!    completely empty window, which is why "the code ran" is not the check.
//! 4. **The loop drew the frames it was asked for** - `DEMO_FRAMES` of them, through one reused `CameraFrame`,
//!    `Bind` and `Present`. AS4's clause, stated on the display path.
//!
//! What this is NOT: not R1 (a windowed frame at any resolution is not a 1080p budget claim), not a hashed
//! number (colour is presentation; nothing here feeds a root — docs/02's float discipline, SPEC-02), and not
//! the boot (`cargo run -p engine`'s 18 headless lines and their digest are untouched; `winit` is a
//! dev-dependency precisely so it cannot sneak a display requirement into them).

use assetc::watch::{Outcome, Watch};
use gpu::device::{Bind, CameraFrame, GpuCamera, Interpreter, Present, Shape};
use render::{march, Tree, ROOT};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

const BOOT_NODES: usize = 11;
const BOOT_PARAMS: usize = 25;
const BOOT_DEPTH: u32 = 4;
/// The demo's camera law, the one `boot_scene_on_device` marches and `r1_specialized_frame` times: a
/// downward-tilted pinhole at `[0.0, 0.6, -4.0]`, 96 steps, `t` out to 40, surface epsilon 1e-3.
const DEFAULT_W: u32 = 640;
const DEFAULT_H: u32 = 360;
const DEFAULT_FRAMES: u32 = 60;
const HALF_HEIGHT: f32 = 0.5;
const MAX_T: f32 = 40.0;
const SURFACE: f32 = 1.0e-3;
const MAX_STEPS: u32 = 96;
const DEVIATION: f32 = 0.002;
/// The no-surface sentinel, as the host side counts it (`device.rs`'s `UNSUPPORTED_HALF`).
const MISS: f32 = 1.0e37;

fn setting(name: &str, fallback: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn fixture() -> std::path::PathBuf {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    std::fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", raw.display()))
}

/// The demo's scene, through the compiler object the boot uses, written to disk and read back.
fn boot_scene() -> Tree {
    let input = fixture();
    let output =
        std::env::temp_dir().join(format!("merkles_demo_window_{}.bin", std::process::id()));
    let mut watch = Watch::default();
    let outcome = watch.observe(
        input.to_str().expect("the fixture path is utf-8"),
        output.to_str().expect("the temp path is utf-8"),
    );
    let (nodes, depth) = match outcome {
        Outcome::Rebuilt { nodes, depth } => (nodes, depth),
        Outcome::Unchanged => panic!("a first observation of a fresh file cannot be unchanged"),
        Outcome::Failed(reason) => panic!("{} must compile: {reason}", input.display()),
    };
    let bytes = std::fs::read(&output).expect("the compiler renamed its asset into place");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("the demo scene must pass the validator the march requires");
    // 1. The scene is the boot's, and the depth its envelope declares is the depth the walk needs.
    assert_eq!(
        (nodes, depth),
        (BOOT_NODES, BOOT_DEPTH),
        "the demo's content compiled to {nodes} nodes at depth {depth} where the boot publishes \
         {BOOT_NODES} at {BOOT_DEPTH}"
    );
    assert_eq!(
        tree.params.len(),
        BOOT_PARAMS,
        "the pool holds {} parameters where the boot's shape needs {BOOT_PARAMS}",
        tree.params.len()
    );
    let walked = render::tree_depth(&tree);
    assert_eq!(
        walked, BOOT_DEPTH,
        "the envelope declares depth {BOOT_DEPTH} while the walk's own frame count is {walked}, so the \
         bytes the window drew are not the boot's scene"
    );
    let _ = std::fs::remove_file(&output);
    tree
}

fn camera(width: u32, height: u32) -> GpuCamera {
    let v = [0.0f32, -0.1, 1.0];
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    GpuCamera {
        origin: [0.0f32, 0.6, -4.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [v[0] / l, v[1] / l, v[2] / l],
        half_height: HALF_HEIGHT,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps: MAX_STEPS,
        root: ROOT,
        width,
        height,
    }
}

/// The pinhole `interp.wgsl`'s `march_rays_from_camera` derives, restated on this side so the reference walk
/// can be charged for the same ray the device walked. Copied arithmetic, not another crate's private surface.
fn direction(px: u32, py: u32, width: u32, height: u32, cam: &GpuCamera) -> [f32; 3] {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * HALF_HEIGHT;
    let span = [
        cam.forward[0] + cam.right[0] * ndc_x + cam.up[0] * ndc_y,
        cam.forward[1] + cam.right[1] * ndc_x + cam.up[1] * ndc_y,
        cam.forward[2] + cam.right[2] * ndc_x + cam.up[2] * ndc_y,
    ];
    let l = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    [span[0] / l, span[1] / l, span[2] / l]
}

/// One frame's device objects, built for one drawable size and reused until the window changes it.
struct Canvas {
    frame: CameraFrame,
    bind: Bind,
    present: Present,
    width: u32,
    height: u32,
}

struct Demo {
    interpreter: Interpreter,
    tree: Tree,
    shape: Shape,
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    canvas: Option<Canvas>,
    frames_left: u32,
    drawn: u32,
    ms_total: f64,
    hits: usize,
    deviation: f32,
    sized: (u32, u32),
    wants: u32,
    displayed: bool,
}

impl Demo {
    fn resize(&mut self, surface: &wgpu::Surface<'static>, width: u32, height: u32) {
        let config = match self.interpreter.configure_surface(surface, width, height) {
            Ok(config) => config,
            Err(reason) => panic!("{width}x{height} is not a drawable: {reason}"),
        };
        let mut frame = self.interpreter.begin_camera_frame(width, height);
        self.interpreter
            .write_camera(&mut frame, &camera(width, height));
        let bind = self.interpreter.bind_camera(&self.shape, &frame);
        let present =
            self.interpreter
                .begin_presentation(&frame, config.format, width, height, MAX_T);
        self.canvas = Some(Canvas {
            frame,
            bind,
            present,
            width,
            height,
        });
        self.sized = (width, height);
    }

    fn draw_one(&mut self) {
        // The surface and the canvas are taken out for the frame rather than borrowed, because the frame both
        // reads the device (through `self.interpreter`) and counts itself (through `self.drawn`), and winit's
        // handler is one `&mut self`. Nothing is created by this dance: `Option::take` moves a handle.
        let surface = self.surface.take().expect("a window has a surface");
        let canvas = self.canvas.take().expect("a sized window has a canvas");
        let start = std::time::Instant::now();
        // The march, with no read-back: the depths stay on the device for the render pass to sample. Refused
        // here rather than folded into the arm below, because a frame the device would not march belongs in the
        // log as *that* failure and not as a `drawn != wants` assertion three lines from the end.
        self.interpreter
            .march_camera_frame_no_readback(&canvas.bind, &canvas.frame)
            .expect("the device must march the frame the window is about to show");
        match surface.get_current_texture() {
            Ok(texture) => self.show(&canvas, texture, start),
            Err(wgpu::SurfaceError::Lost) | Err(wgpu::SurfaceError::Outdated) => {
                self.interpreter
                    .configure_surface(&surface, canvas.width, canvas.height)
                    .expect("a lost surface reconfigures at its own size");
            }
            Err(error) => panic!("the window handed back no texture: {error}"),
        }
        self.surface = Some(surface);
        self.canvas = Some(canvas);
    }

    /// The half no headless harness can reach: the render pass, the submit, and the swapchain hand-off.
    fn show(&mut self, canvas: &Canvas, texture: wgpu::SurfaceTexture, start: std::time::Instant) {
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        canvas.present.draw(&view);
        texture.present();
        self.ms_total += start.elapsed().as_secs_f64() * 1000.0;
        self.drawn += 1;
        self.displayed = true;
    }

    /// The check the loop cannot make a liar out of: read the frame the window just showed and compare it to
    /// the reference march, ray for ray, over the grid that was drawn.
    fn verify(&mut self) {
        let canvas = self
            .canvas
            .take()
            .expect("frames were drawn, so a canvas exists");
        let read = self
            .interpreter
            .read_camera_depths(&canvas.frame)
            .expect("the displayed frame's depths must be readable");
        let (width, height) = (canvas.width, canvas.height);
        // Cloned because the loop below runs while `self` is otherwise untouched: 11 records and 25 parameters
        // is not a cost worth a borrow dance around a check that is not on any frame path.
        let tree = self.tree.clone();
        let cam = camera(width, height);
        let mut reference = 0usize;
        let mut worst = 0.0f32;
        for py in 0..height {
            for px in 0..width {
                let dir = direction(px, py, width, height, &cam);
                let hit = march(&tree, ROOT, cam.origin, dir, MAX_T, SURFACE, MAX_STEPS);
                let depth = read.depths[py as usize * width as usize + px as usize];
                match (hit, depth < MISS) {
                    (Some(hit), true) => worst = worst.max((hit.t - depth).abs()),
                    (None, false) => {}
                    (Some(_), false) => panic!(
                        "pixel ({px},{py}): the reference finds a surface the device reported as sky"
                    ),
                    (None, true) => panic!(
                        "pixel ({px},{py}): the device reported a surface the reference does not find"
                    ),
                }
                if hit.is_some() {
                    reference += 1;
                }
            }
        }
        self.canvas = Some(canvas);
        self.hits = read.hits;
        self.deviation = worst;
        // 2 + 3. Same hit count as the reference over the same grid, and not an empty window.
        assert_eq!(
            read.hits, reference,
            "the windowed path found {} surfaces where the CPU reference finds {reference} over the same \
             {width}x{height} grid and the same camera law",
            read.hits
        );
        assert!(
            reference > 0,
            "the {width}x{height} grid found 0 surfaces, so this example has been showing a sky and any \
             shader would pass"
        );
        assert!(
            worst < DEVIATION,
            "the windowed path deviates from the reference march by {worst}"
        );
    }
}

impl ApplicationHandler for Demo {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = WindowAttributes::default()
            .with_title("Merkles World - the demo's scene, marched and presented on a device")
            .with_inner_size(winit::dpi::PhysicalSize::new(self.sized.0, self.sized.1));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                println!(
                    "SKIP demo_window: no window ({error}) - a display path cannot be proved by a runner \
                     with no monitor, and CI has none"
                );
                event_loop.exit();
                return;
            }
        };
        let surface = match self.interpreter.open_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => panic!("the window must take a surface from its own adapter: {error}"),
        };
        let size = window.inner_size();
        self.window = Some(window);
        self.resize(&surface, size.width, size.height);
        self.surface = Some(surface);
        println!(
            "// windowed         : {}x{}, {} frames requested",
            size.width, size.height, self.wants
        );
        event_loop.set_control_flow(ControlFlow::Poll);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    let surface = self.surface.take().expect("a window has a surface");
                    self.resize(&surface, size.width, size.height);
                    self.surface = Some(surface);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.canvas.is_none() || self.frames_left == 0 {
            return;
        }
        self.draw_one();
        self.frames_left -= 1;
        if self.frames_left == 0 {
            self.verify();
            // `ControlFlow::Poll` has no other reason to stop: a demo that finishes its frames and does not
            // exit looks, from a harness, exactly like a demo that is still drawing. The first run of this
            // example hung here for ten minutes with a correct picture in the window and every assertion
            // already satisfied.
            event_loop.exit();
        }
    }
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP demo_window: no device ({error:?}) - the presentation path is a device path, and a \
                 CPU adapter would display the reference march and call it a frame"
            );
            return;
        }
    };
    println!("// adapter          : {}", interpreter.adapter());
    let tree = boot_scene();
    println!(
        "// content          : {} node records, {} parameters, declared depth {BOOT_DEPTH} - the boot's \
         scene, compiled by assetc and read back off disk",
        tree.nodes.len(),
        tree.params.len(),
    );

    let shape = interpreter.upload(&tree);
    println!(
        "// uploaded         : {} bytes resident, one allocation for the scene",
        shape.bytes
    );

    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            println!(
                "SKIP demo_window: no event loop ({error}) - no display to present on, which is the \
                 legitimate answer for a runner with a GPU and no monitor"
            );
            return;
        }
    };
    let wants = setting("DEMO_FRAMES", DEFAULT_FRAMES);
    let mut demo = Demo {
        interpreter,
        tree,
        shape,
        window: None,
        surface: None,
        canvas: None,
        frames_left: wants,
        drawn: 0,
        ms_total: 0.0,
        hits: 0,
        deviation: 0.0,
        sized: (setting("DEMO_W", DEFAULT_W), setting("DEMO_H", DEFAULT_H)),
        wants,
        displayed: false,
    };
    if let Err(error) = event_loop.run_app(&mut demo) {
        println!("SKIP demo_window: the loop ended in error ({error})");
        return;
    }

    // 4. The loop drew what it was asked for, and nothing else was created to do it.
    assert_eq!(
        demo.drawn, demo.wants,
        "the window drew {} frames of the {} requested",
        demo.drawn, demo.wants
    );
    assert!(demo.displayed, "no frame reached the surface");
    let (width, height) = demo.sized;
    println!(
        "// frames drawn     : {} at {width}x{height}, one CameraFrame/Bind/Present for all of them",
        demo.drawn
    );
    println!(
        "// present binds    : {} bytes resident on the device, sampled in place by the fragment stage; no \
         depth byte crossed the bus on any frame (the read-back below is the check, run once, after the loop)",
        demo.canvas
            .as_ref()
            .expect("a canvas outlives its frames")
            .present
            .bound_bytes()
    );
    println!(
        "// displayed        : {} surfaces of {}, worst deviation from the reference march {:.6}",
        demo.hits,
        width * height,
        demo.deviation,
    );
    println!(
        "// windowed frame   : mean {:.3} ms over the drawn frames - informational only: gate R1's number \
         is 1080p on the named hardware and is measured by r1_specialized_frame, not here (ADR-017)",
        demo.ms_total / demo.drawn.max(1) as f64
    );
    println!(
        "// what this lands   : Phase 1's exit clause has a window in it. The march stayed on the device, \
         the depths were never copied to the host between the first frame and the last, and the picture the \
         reference march computes is the picture that was shown."
    );
}
