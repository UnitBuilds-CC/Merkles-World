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
            eprintln!("usage: assetc <input.ron> <output.bin>");
            eprintln!("       assetc watch [--once] [--interval <ms>] <input.ron> <output.bin>");
            std::process::exit(2);
        }
        Some(Command::Build { input, output }) => {
            let mut watcher = Watch::default();
            match watcher.observe(&input, &output) {
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
            if args.len() > 2 {
                return None;
            }
            return Some(Command::Build {
                input: input.clone(),
                output: output.clone(),
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

const DEFAULT_INTERVAL_MS: u64 = 50;
