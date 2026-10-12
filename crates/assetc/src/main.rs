//! `assetc` - the compiler command doc 09 promises: an authored shape file in, a `ShapeAsset` out.
//!
//! Everything interesting about this binary is what it refuses to do. It does not parse at runtime
//! (AGENTS.md §3 keeps string parsing off the frame path, so the engine loads bytes and never sees
//! RON), it does not guess a shape it cannot evaluate (a kind `crates/render` refuses is an error
//! here, not a silently approximated node), and it does not emit a file its own reader would be the
//! only witness for - CI feeds the output to `flatc`, which is what makes the format public.
//!
//! Usage: `assetc <input.ron> <output.bin>`

use assetc::writer::to_bytes;
use assetc::{lower, Shape};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: assetc <input.ron> <output.bin>");
        std::process::exit(2);
    };

    match compile(&input, &output) {
        Ok((nodes, depth)) => println!("{input} -> {output}: {nodes} node(s), depth {depth}"),
        Err(message) => {
            eprintln!("{input}: {message}");
            std::process::exit(1);
        }
    }
}

fn compile(input: &str, output: &str) -> Result<(usize, u32), String> {
    let text = std::fs::read_to_string(input).map_err(|e| format!("cannot read: {e}"))?;
    let authored: Shape =
        ron::from_str(&text).map_err(|e| format!("not a valid authoring file: {e}"))?;
    if authored.node_count() > u16::MAX as usize {
        return Err(format!(
            "too many nodes for a 16-bit parameter pool: {}",
            authored.node_count()
        ));
    }
    let asset = lower(&authored).map_err(|e| format!("refused by the compiler: {e:?}"))?;
    let label = std::path::Path::new(input)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "asset".to_string());
    let bytes = to_bytes(&asset, &label);
    std::fs::write(output, &bytes).map_err(|e| format!("cannot write: {e}"))?;
    Ok((asset.tree.nodes.len(), asset.depth))
}
