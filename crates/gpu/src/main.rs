//! What this machine can actually see: enumerate the adapters `wgpu` would run the renderer on.
//!
//! Spike S9 asks what `wgpu` costs versus raw WebGPU, and spike S6 asks whether a WGSL program can be
//! validated headlessly in CI. Both questions were unanswerable in the abstract, so this binary is the
//! measurement, on the branch, before anything lands on `main`.
//!
//! **Measured on 2026-10-12, x86-64 Windows, adding `wgpu = "27"` to the workspace:**
//!
//! - the resolved third-party graph goes from **34 packages to 104** - 66 new, including `naga`, `ash`,
//!   `d3d12`, `metal`, `glow`, `gpu-alloc` and the `windows` crate;
//! - a cold build of the dependency graph took **132 s**, and `target/debug` grew from 1,160 MB to
//!   1,904 MB (+744 MB) for a binary that does nothing yet but enumerate;
//! - `deny.toml`'s allow list does not cover everything the graph brings - but the guess made here was
//!   wrong and CI corrected it. The job's whole licence finding was **one rejection: `hexf-parse`,
//!   `CC0-1.0`, via `naga`**. The slash-form licences (`khronos-egl`, `rustc-hash`, written
//!   `MIT/Apache-2.0`) cargo-deny reads as alternatives, so they pass; only the public-domain
//!   dedication is outside the list. The second failure was not a licence at all: **`paste` is flagged
//!   unmaintained**, an advisory rather than a policy gap, and it is the same category of thing as
//!   BLAKE3's SIMD build - a transitive crate whose maintenance status is somebody's call, not this
//!   branch's. Three duplicate-version warnings (`hashbrown` x3, `foldhash`, `syn`) and a
//!   future-incompat note about `block v0.1.6` came along too.
//!
//! **The hypothesis about CI came back wrong in one place and right in two, which is the finding.** PR
//! #1 ran this probe on all three gate runners (`38258220750`):
//!
//! - **x86-64 Windows** - adapters present, `request_device` **opened on Dx12** (no GPU on a shared
//!   runner, so that is a software device);
//! - **ARM64 macOS** - one adapter, `Apple Paravirtual device` on **Metal**, and it **opened**;
//! - **x86-64 Linux** - **no adapters at all**: the probe printed its own refusal, "no device means no
//!   frame curve".
//!
//! So a WGSL compute program can be run against the CPU reference in CI on two of the three
//! architectures, which is spike S6's missing harness - it was being written off as impossible without
//! hardware. It cannot on the Linux runner, so an agreement check there has to skip, and a skip must
//! never be reported as a passing gate. None of it makes R1 measurable: a paravirtual or software
//! device is the wrong instrument for a frame budget, as is this machine's GeForce MX250, which is below
//! docs/04's "mid-range dGPU" rather than an example of it.
//!
//! And what landing `wgpu` would cost is in the list above, measured rather than estimated. The
//! `cargo-deny` job on this branch is what replaced the guesswork: it failed, with one licence
//! rejection and one unmaintained-crate advisory, and both are policy for OQ-43 and ADR-025 to absorb -
//! which is why this is a pull request with a red job on purpose rather than a merge with the list
//! quietly widened.

//!
//! Limits matter for the shape of the march, so they are printed rather than assumed:
//! `max_compute_workgroup_size_x` is 1,024 on every adapter here except the NVIDIA one under Vulkan
//! (1,536), `max_storage_buffer_binding_size` is 2 GiB everywhere - so docs/09's compiled asset is one
//! binding by a margin of six orders of magnitude, and the AS4 zero-copy question is not a capacity
//! question. Note also what is *here*: a GeForce MX250 counts as a DiscreteGpu, but docs/04's budget is
//! written against "a mid-range dGPU", and an MX250 is below that rather than at it - so a curve
//! measured on this machine has to be labelled as one, not quoted as the gate's answer.
//!
//! Run: `cargo run -p gpu`

fn main() {
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());

    println!(
        "// adapters visible to this process on {} / {} (wgpu as pinned in Cargo.lock)",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if adapters.is_empty() {
        println!("none. No device means no frame curve: gate R1 cannot be measured here.");
        return;
    }
    for adapter in &adapters {
        let info = adapter.get_info();
        println!(
            "  {:24} backend {:?}  device {:?}  vendor {:?}  type {:?}",
            info.name, info.backend, info.device, info.vendor, info.device_type
        );
        // The limits are the ones docs/04's budget assumes are free.
        let limits = adapter.limits();
        println!(
            "    max_compute_workgroup_size_x = {}, max_buffer_size = {} bytes, max_storage_buffer_binding_size = {} bytes",
            limits.max_compute_workgroup_size_x, limits.max_buffer_size, limits.max_storage_buffer_binding_size
        );
    }

    // Asking for a device is the real test: an adapter that cannot open is not a renderer. The
    // descriptor is completed with `..Default::default()` rather than field by field, because wgpu
    // adds fields to it between minor versions and a probe that fails to compile is worse than a
    // probe that asks for nothing special.
    let device_result = pollster::block_on(adapters[0].request_device(&wgpu::DeviceDescriptor {
        label: Some("probe"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..Default::default()
    }));
    match device_result {
        Ok((_device, _queue)) => println!(
            "device            : opened on {:?}",
            adapters[0].get_info().backend
        ),
        Err(error) => println!("device            : could not be opened - {error}"),
    }
}
