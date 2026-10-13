//! The half of spike S6 that needs no GPU: does A-07's WGSL compile and validate at all?
//!
//! This was written off as needing hardware. It does not: `naga` is the WGSL front end `wgpu` itself
//! uses, and it parses and validates a module entirely on the CPU, so "the shader the renderer would run
//! is valid WGSL" is checkable on every architecture in CI - including the x86-64 Linux runner, where
//! the adapter probe in `main.rs` found no device at all (PR #1, run `38258220750`).
//!
//! What this does **not** do is execute a single shader instruction. No distances come back, so nothing
//! here may be quoted as agreement between the two implementations, as spike S4's zero-copy upload, or
//! as gate R1's frame budget. Those need a device, which two of three runners can provide and one
//! cannot, and the harness for that is the unfinished part of this branch.
//!
//! The binding indices and the entry point are asserted against the source text rather than against
//! naga's internal representation, deliberately: the IR's field names are the kind of thing that moves
//! between minor versions, and a test that breaks on an upgrade without the shader having changed is
//! worse than a test with a blunt instrument. The strict version of this check is a device reading the
//! buffers, which is exactly the part still missing.

/// The WGSL source, the same file `wgpu` would be handed.
pub const INTERP: &str = include_str!("interp.wgsl");

/// Why validation failed, in the words naga gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

/// Parse and validate a WGSL module. `Ok` means naga's full validation passes: types, bindings,
/// control flow, and the constant expressions the shader depends on.
pub fn validate(source: &str) -> Result<(), ValidationError> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| ValidationError(error.emit_to_string(source)))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .map_err(|error| ValidationError(format!("{error:?}")))?;
    Ok(())
}

/// The interface the CPU side has to match, checked where it is stated: in the source.
///
/// Returns the `@binding(...)` indices the module declares, in the order they appear.
pub fn declared_bindings(source: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for line in source.lines() {
        if let Some(rest) = line.trim().strip_prefix("@group(0) @binding(") {
            if let Some(digits) = rest.split(')').next() {
                if let Ok(index) = digits.trim_end_matches(',').parse::<u32>() {
                    out.push(index);
                }
            }
        }
    }
    out
}
