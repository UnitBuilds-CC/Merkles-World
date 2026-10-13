//! Why does an enabled backend enumerate zero adapters? Separates "not compiled in" from
//! "compiled in but the platform declined it", which is the difference between a build problem and a
//! headless-runtime problem. Measured this way on buildbox (headless Ubuntu, GTX 1070, driver 580.178.04):
//! `enabled_backend_features = VULKAN | GL`, and VULKAN, GL and ALL each enumerate **0** adapters while
//! `vulkaninfo --summary` in the same shell and the same group lists the NVIDIA device plus llvmpipe. On
//! this Windows laptop the same code enumerates seven adapters across Vulkan, Dx12 and Gl.

fn main() {
    println!(
        "// compiled-in backends = {:?}",
        wgpu::Instance::enabled_backend_features()
    );
    let inst = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    for (name, b) in [
        ("VULKAN", wgpu::Backends::VULKAN),
        ("GL", wgpu::Backends::GL),
        ("ALL", wgpu::Backends::all()),
    ] {
        let adapters = inst.enumerate_adapters(b);
        println!("{}: {} adapter(s)", name, adapters.len());
        for a in adapters {
            let i = a.get_info();
            println!("   {} | {:?} | {:?}", i.name, i.backend, i.device_type);
        }
    }
}
