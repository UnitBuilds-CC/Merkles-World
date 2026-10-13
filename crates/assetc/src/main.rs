//! `assetc` - the compiler command doc 09 promises: an authored shape file in, a `ShapeAsset` out.
//!
//! Everything interesting about this binary is what it refuses to do. It does not parse at runtime
//! (AGENTS.md §3 keeps string parsing off the frame path, so the engine loads bytes and never sees RON),
//! it does not guess a shape it cannot evaluate (a kind `crates/render` refuses is an error here, not a
//! silently approximated node), and it does not emit a file its own reader would be the only witness for
//! - CI feeds the output to `flatc`, which is what makes the format public.
//!
//! Usage:
//! ```text
//! assetc <input.ron> <output.bin>
//! assetc watch [--once] [--interval <ms>] <input.ron> <output.bin>
//! ```
//!
//! `watch` is the dev-profile loop: recompile when the authoring bytes change, replace the asset
//! atomically, and keep the last good asset when an edit does not compile. `--once` performs a single
//! observation and exits non-zero if it failed, which is the form the tests and CI can use - a loop that
//! must be waited on is a loop whose assertions race it.

use assetc::watch::{Outcome, Watch};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Command::parse(&args) {
        None => {
            eprintln!("usage: assetc <input.ron> <output.bin> [--budget nodes=N,depth=D,evaluations=E[,steps=S]]");
            eprintln!("       assetc watch [--once] [--interval <ms>] <input.ron> <output.bin>");
            std::process::exit(2);
        }
        Some(Command::Build {
            input,
            output,
            budget,
            steps,
        }) => {
            let mut watcher = Watch::default();
            match watcher.observe_with(&input, &output, budget.map(|limit| (limit, steps))) {
                Outcome::Rebuilt { nodes, depth } => {
                    println!("{input} -> {output}: {nodes} node(s), depth {depth}");
                }
                Outcome::Unchanged => unreachable!("a fresh Watch has nothing compiled"),
                Outcome::Failed(message) => {
                    eprintln!("{input}: {message}");
                    std::process::exit(1);
                }
            }
        }
        Some(Command::Watch {
            input,
            output,
            once,
            interval_ms,
        }) => {
            let mut watcher = Watch::default();
            loop {
                match watcher.observe(&input, &output) {
                    Outcome::Rebuilt { nodes, depth } => {
                        println!("watch: {input} -> {output}: {nodes} node(s), depth {depth}");
                    }
                    Outcome::Unchanged => {
                        if once {
                            println!("watch: unchanged since the last compile, nothing written");
                        }
                    }
                    Outcome::Failed(message) => {
                        // The loop survives a broken edit; a single pass does not, so `--once` can be
                        // asserted on. Either way the previous asset stays on disk and says so here.
                        eprintln!("watch: {input}: {message}; the previous asset is unchanged");
                        if once {
                            std::process::exit(1);
                        }
                    }
                }
                if once {
                    break;
                }
                std::thread::sleep(core::time::Duration::from_millis(interval_ms));
            }
        }
    }
}

enum Command {
    Build {
        input: String,
        output: String,
        budget: Option<assetc::budget::Budget>,
        steps: u32,
    },
    Watch {
        input: String,
        output: String,
        once: bool,
        interval_ms: u64,
    },
}

impl Command {
    fn parse(args: &[String]) -> Option<Command> {
        if args.is_empty() {
            return None;
        }
        if args[0] != "watch" {
            let (Some(input), Some(output)) = (args.first(), args.get(1)) else {
                return None;
            };
            let mut budget = None;
            let mut steps = DEFAULT_STEP_BUDGET;
            let mut positional = 2usize;
            let mut index = 2usize;
            while index < args.len() {
                match args[index].as_str() {
                    "--budget" => {
                        index += 1;
                        let (parsed, parsed_steps) =
                            args.get(index).map(String::as_str).and_then(parse_budget)?;
                        budget = Some(parsed);
                        steps = parsed_steps;
                    }
                    _ => return None,
                }
                index += 1;
                positional = index;
            }
            if positional != args.len() {
                return None;
            }
            let _ = positional;
            return Some(Command::Build {
                input: input.clone(),
                output: output.clone(),
                budget,
                steps,
            });
        }
        let mut once = false;
        let mut interval_ms = DEFAULT_INTERVAL_MS;
        let mut positional = Vec::new();
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--once" => once = true,
                "--interval" => {
                    index += 1;
                    let raw = args.get(index)?;
                    interval_ms = raw.parse().ok()?;
                }
                other => positional.push(other.to_string()),
            }
            index += 1;
        }
        if positional.len() != 2 {
            return None;
        }
        Some(Command::Watch {
            input: positional.remove(0),
            output: positional.remove(0),
            once,
            interval_ms,
        })
    }
}

/// The raster's own default, so a budget stated without a step count means "at the step budget the CPU
/// reference actually marches at" rather than silently assuming one.
const DEFAULT_STEP_BUDGET: u32 = 96;

/// `--budget nodes=32,depth=8,evaluations=6144[,steps=96]`.
///
/// Every clause but `steps` is required, on purpose: an evaluation ceiling is meaningless without the step
/// count it is stated against, and defaulting it silently would let two people write the same flag with
/// different meanings.
fn parse_budget(raw: &str) -> Option<(assetc::budget::Budget, u32)> {
    let mut nodes: Option<u32> = None;
    let mut depth: Option<u32> = None;
    let mut evaluations: Option<u64> = None;
    let mut steps = DEFAULT_STEP_BUDGET;
    for part in raw.split(',') {
        let (key, value) = part.split_once('=')?;
        match key.trim() {
            "nodes" => nodes = Some(value.trim().parse::<u32>().ok()?),
            "depth" => depth = Some(value.trim().parse::<u32>().ok()?),
            "evaluations" => evaluations = Some(value.trim().parse::<u64>().ok()?),
            "steps" => steps = value.trim().parse::<u32>().ok()?,
            _ => return None,
        }
    }
    Some((
        assetc::budget::Budget {
            max_nodes: nodes?,
            max_depth: depth?,
            max_evaluations_per_ray: evaluations?,
        },
        steps,
    ))
}

const DEFAULT_INTERVAL_MS: u64 = 50;
