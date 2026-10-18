fn main() {
    let downlevel = wgpu::Limits::downlevel_defaults();
    println!("Downlevel defaults:");
    println!("  max_bind_groups: {}", downlevel.max_bind_groups);
    println!("  max_storage_buffers_per_shader_stage: {}", downlevel.max_storage_buffers_per_shader_stage);
    println!("  max_storage_buffer_binding_size: {}", downlevel.max_storage_buffer_binding_size);
    
    let default_limits = wgpu::Limits::default();
    println!("\nDefault limits:");
    println!("  max_bind_groups: {}", default_limits.max_bind_groups);
    println!("  max_storage_buffers_per_shader_stage: {}", default_limits.max_storage_buffers_per_shader_stage);
    println!("  max_storage_buffer_binding_size: {}", default_limits.max_storage_buffer_binding_size);
}
