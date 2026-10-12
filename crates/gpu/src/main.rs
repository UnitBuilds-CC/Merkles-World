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
//! - `deny.toml`'s allow list does not cover everything the graph brings. Read with cargo-deny's own
//!   rule - an `OR` is satisfied by any one allowed operand, an `AND` needs all of them - the gaps are
//!   `hexf-parse` (**CC0-1.0**, only, via `naga`), `khronos-egl` and `rustc-hash` (written as
//!   `MIT/Apache-2.0`, a slash form the SPDX expression parser does not read as an `OR`), and
//!   `unicode-ident` (`(MIT OR Apache-2.0) AND Unicode-3.0`), already in the graph and already waived.
//!   None of those is copyleft; all of them are a licence-policy widening, which is OQ-43's decision
//!   and not this branch's. CI's `cargo-deny` job is the authority on what it actually rejects, which
//!   is why this branch exists as a pull request rather than a merge.
//!
//! **And the finding that changes the roadmap:** this machine exposes a `Microsoft Basic Render Driver`
//! adapter with `device_type: Cpu` alongside the Intel UHD and NVIDIA MX250, and `request_device`
//! succeeds (on Vulkan here). GitHub's Windows runners have no GPU, so the hypothesis worth testing - and
//! the reason this is a pull request rather than a paragraph - is that the same software rasteriser shows
//! up there, which would mean **S6's headless WGSL check and a GPU-versus-CPU agreement test can run in CI
//! at all.** They have been written off as needing hardware the harness does not have. If the runner's
//! adapter list comes back empty, S6 needs a self-hosted machine and this branch says so instead of
//! pretending otherwise. Either way it does not make R1 measurable there: a CPU rasteriser is precisely
//! the thing that cannot answer a frame-budget question, and a number from it must never be quoted as one.
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
