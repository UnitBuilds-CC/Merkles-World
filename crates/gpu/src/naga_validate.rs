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

/// The names of the storage globals declared under `@group(1)` — the temporal cache's buffers.
fn cache_globals(source: &str) -> Vec<&str> {
    let mut names = Vec::new();
    for line in source.lines() {
        if line.trim().strip_prefix("@group(1) @binding(").is_none() {
            continue;
        }
        let Some(var) = line.find("var<") else {
            continue;
        };
        let after_qualifier = &line[var + 4..];
        let Some(gt) = after_qualifier.find('>') else {
            continue;
        };
        let name = after_qualifier[gt + 1..]
            .split(':')
            .next()
            .unwrap_or("")
            .trim();
        if !name.is_empty() {
            names.push(name);
        }
    }
    names
}

/// Which `@compute` entry point owns the `@group(1)` cache group, or `None` when nothing reaches it.
///
/// This exists because `wgpu` derives a `layout: None` pipeline's bind group layouts from the resources the
/// entry point it was compiled *for* references. A camera march that never names the cache therefore has a
/// one-group layout, and asking its pipeline for group 1 is a device-side validation error — `device.rs`'s
/// camera binding used to ask unconditionally, which every GPU would have caught and no CI runner without a
/// GPU could: the instruments that bind these pipelines print `SKIP … NoAdapter` on the Linux and Windows
/// legs, so only the ARM64 macOS adapter and real hardware ever reached the call.
///
/// A module whose cache is reached through a shared helper, or by two entry points, is refused rather than
/// guessed at: attributing a layout to an entry point then needs the call graph, and the honest price of that
/// refactor is to say so here instead of binding a group the pipeline never derived.
pub fn temporal_cache_owner(source: &str) -> Result<Option<&str>, String> {
    let globals = cache_globals(source);
    let mut owner: Option<&str> = None;
    let mut body: Option<&str> = None;
    let mut body_is_entry = false;
    let mut next_is_entry = false;
    for line in source.lines() {
        if line.starts_with("@compute") {
            next_is_entry = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("fn ") {
            body = rest.split('(').next();
            body_is_entry = next_is_entry;
            next_is_entry = false;
            continue;
        }
        if line == "}" {
            body = None;
            body_is_entry = false;
            continue;
        }
        let Some(reader) = body else { continue };
        if !globals.iter().any(|global| line.contains(*global)) {
            continue;
        }
        if !body_is_entry {
            return Err(format!(
                "`{reader}` reads the @group(1) cache but is not a @compute entry point, so no bind group \
                 layout can be attributed to an entry point from this module without following calls"
            ));
        }
        if let Some(other) = owner.filter(|other| *other != reader) {
            return Err(format!(
                "both `{other}` and `{reader}` read the @group(1) cache, so which pipeline owns that group is \
                 not something this module states once"
            ));
        }
        owner = Some(reader);
    }
    Ok(owner)
}
