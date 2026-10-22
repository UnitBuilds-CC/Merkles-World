//! `assetc watch`, the reload loop doc 09's dev profile promises.
//!
//! The interesting part is not the polling - a compiler that re-runs when a file changes is ordinary. It
//! is what happens when the edit is *wrong*. A watch loop that recompiles a broken authoring file and
//! leaves the last good asset in place is a lie waiting to be rendered: the engine loads bytes, the author
//! sees the old shape, and the mismatch gets blamed on something else later. So the two rules here are:
//!
//! 1. **Replace atomically or not at all.** Bytes go to `<output>.tmp` first and are renamed into place
//!    only once the compile succeeded, so a reader racing this loop cannot observe a half-written asset. A
//!    truncated asset would fail `reader::from_bytes` loudly, which is the good outcome; an asset that
//!    parses but is *stale* is the bad one, and compile-before-write is what makes that pair impossible.
//! 2. **A failed compile keeps the previous asset and says so.** The loop survives it - that is the point
//!    of a watch - but the outcome is a distinct [`Outcome::Failed`], never a silent success, and never a
//!    write.
//!
//! Change detection is a 64-bit FNV-1a over the authoring text rather than a file mtime, because mtimes
//! are coarse enough to miss an edit that lands in the same clock tick and loose enough to fire on a save
//! that changed nothing. The hash answers the question actually asked: did the bytes differ. Nothing here
//! feeds a Merkle root (docs/02's discipline is not what this note is about) - it is a checksum over a
//! file a human is editing.

use crate::reader::from_bytes;
use crate::writer::to_bytes;
use crate::{compile_entity, lower, EntityDef, Shape};

/// What one observation of the authoring file decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The authoring text differs from what was last compiled, and the asset was replaced.
    Rebuilt { nodes: usize, depth: u32 },
    /// The authoring text is the bytes already compiled. Nothing was written.
    Unchanged,
    /// Compilation or I/O failed. The asset on disk is whatever succeeded last, untouched.
    Failed(String),
}

/// Remembers what was last compiled, so a repeated observation can answer [`Outcome::Unchanged`].
#[derive(Debug, Default)]
pub struct Watch {
    seen: Option<u64>,
}

impl Watch {
    /// Observe `input` once, replacing `output` if and only if it changed and compiles.
    pub fn observe(&mut self, input: &str, output: &str) -> Outcome {
        self.observe_with(input, output, None)
    }

    /// The same observation with doc 09's AS5 ceiling applied (gate AS5's mechanism, not its number).
    ///
    /// A budget violation is reported exactly like a compile failure: the asset on disk is untouched, the
    /// fingerprint does not advance, and a later edit that fits is recompiled rather than dismissed as
    /// unchanged. That is not a convenience - an over-budget shape that still replaced the file would make
    /// the compiler's answer depend on which edit happened to be saved last.
    ///
    pub fn observe_with(
        &mut self,
        input: &str,
        output: &str,
        budget: Option<(crate::budget::Budget, u32)>,
    ) -> Outcome {
        let text = match std::fs::read_to_string(input) {
            Ok(text) => text,
            Err(error) => return Outcome::Failed(format!("cannot read: {error}")),
        };
        let stamp = fingerprint(&text);
        if self.seen == Some(stamp) {
            return Outcome::Unchanged;
        }
        // Ordered deliberately: nothing below may touch the output file before this returns Ok.
        let (bytes, cost) = match compile_bytes(input, &text) {
            Ok(compiled) => compiled,
            Err(reason) => return Outcome::Failed(reason),
        };
        if let Some((budget, steps)) = budget {
            let failures = crate::budget::violations(&cost, &budget, steps);
            if !failures.is_empty() {
                let list = failures
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<String>>()
                    .join("; ");
                return Outcome::Failed(format!("over budget {cost:?}: {list}"));
            }
        }
        let summary = match from_bytes(&bytes) {
            Ok(asset) => (asset.nodes.len(), asset.depth),
            // Unreachable for bytes this module just wrote, and if it ever happened the write should not
            // proceed: report instead of shipping an asset the loader would reject.
            Err(error) => {
                return Outcome::Failed(format!("internal: emitted an unreadable asset: {error:?}"))
            }
        };
        if let Err(reason) = atomic_write(output, &bytes) {
            return Outcome::Failed(reason);
        }
        self.seen = Some(stamp);
        Outcome::Rebuilt {
            nodes: summary.0,
            depth: summary.1,
        }
    }

    /// The fingerprint currently considered compiled, if anything is. Useful in a log line when a loop is
    /// resumed, and a seam for tests that want to assert "nothing was recompiled".
    pub fn seen(&self) -> Option<u64> {
        self.seen
    }
}

/// A stable 64-bit FNV-1a over the authoring bytes.
pub fn fingerprint(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Compile already-read authoring text into asset bytes.
///
/// The two formats are tried in order, and a file that fails both reports **both** errors. Reporting only
/// the `Shape` failure sent an `EntityDef` closed by a stray `}` off with "Unexpected variant named
/// `EntityDef` in enum `Shape`" - a message about the format the file is not, which reads as a bad
/// primitive and points the author at the wrong half of the tree.
fn compile_bytes(input: &str, text: &str) -> Result<(Vec<u8>, crate::budget::Cost), String> {
    let entity = ron::from_str::<EntityDef>(text);
    if entity.is_ok() {
        return compile_entity_file(text);
    }
    match ron::from_str::<Shape>(text) {
        Ok(_) => compile_shape_file(input, text),
        Err(shape_error) => Err(format!(
            "not a valid authoring file: as an entity it failed with {}, and as a bare shape with {}",
            entity.unwrap_err(),
            shape_error
        )),
    }
}

/// Compile a Phase 2 entity file (EntityDef wrapper)
fn compile_entity_file(text: &str) -> Result<(Vec<u8>, crate::budget::Cost), String> {
    let def: EntityDef = match ron::from_str(text) {
        Ok(def) => def,
        Err(error) => return Err(format!("not a valid entity file: {error}")),
    };

    let compiled = match compile_entity(&def) {
        Ok(entity) => entity,
        Err(refusal) => return Err(format!("entity schema violation: {refusal}")),
    };

    let cost = crate::budget::cost_of(&compiled.asset);
    let label = "entity".to_string();
    Ok((to_bytes(&compiled.asset, &label), cost))
}

/// Compile a Phase 1 shape file (raw Shape)
fn compile_shape_file(input: &str, text: &str) -> Result<(Vec<u8>, crate::budget::Cost), String> {
    let authored: Shape = match ron::from_str(text) {
        Ok(authored) => authored,
        Err(error) => return Err(format!("not a valid authoring file: {error}")),
    };
    if authored.node_count() > u16::MAX as usize {
        return Err(format!(
            "too many nodes for a 16-bit parameter pool: {}",
            authored.node_count()
        ));
    }
    let asset = match lower(&authored) {
        Ok(asset) => asset,
        Err(refusal) => return Err(format!("schema violation: {refusal}")),
    };
    let cost = crate::budget::cost_of(&asset);
    let label = std::path::Path::new(input)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "asset".to_string());
    Ok((to_bytes(&asset, &label), cost))
}

/// Write `bytes` to a sibling temporary path, then rename over `path`.
///
/// The rename is the point: a reader sees either the old file or the new one. The temporary name derives
/// from the target rather than from a clock or a counter, so two loops watching the same output collide on
/// the same temp file - and whichever loses the race gets an I/O error it reports as
/// [`Outcome::Failed`], rather than both quietly producing a mixed asset.
pub fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), String> {
    let target = std::path::Path::new(path);
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create the output directory: {error}"))?;
        }
    }
    let temp = temp_for(path);
    if let Err(error) = std::fs::write(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot write the temporary asset: {error}"));
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot replace the asset: {error}"));
    }
    Ok(())
}

/// The sibling path `atomic_write` uses. Exposed so a caller (or a test) can assert no temp file was left
/// behind to be mistaken for an asset.
pub fn temp_for(path: &str) -> std::path::PathBuf {
    let target = std::path::Path::new(path);
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "asset.bin".to_string());
    target.with_file_name(format!("{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::compile_bytes;

    /// The exact mistake this reports on: an `EntityDef` whose root node is closed by `}` instead of `)`.
    /// Both formats fail on it, and the author needs the entity half of that.
    const BROKEN_ENTITY: &str = r#"
EntityDef(
    shape: Sphere(r: 1.0, centre: (0.0, 0.0, 0.0)),
    position: (0.0, 0.0, 0.0),
}
"#;

    #[test]
    fn a_broken_entity_reports_the_entity_error_too() {
        let reason = compile_bytes("in.ron", BROKEN_ENTITY)
            .expect_err("an EntityDef closed by `}` must not compile");
        assert!(
            reason.contains("as an entity") && reason.contains("as a bare shape"),
            "the author is told only about the format the file is not: {reason}"
        );
    }

    #[test]
    fn both_formats_still_compile_when_they_are_valid() {
        let authored = include_str!("../../../assets/entities/pig.ron");
        compile_bytes("pig.ron", authored).expect("the shipped pig must compile as an entity file");
        compile_bytes("sphere.ron", "Sphere(r: 0.5, centre: (0.0, 0.0, 0.0))")
            .expect("a bare Phase 1 shape must still compile");
    }

    #[test]
    fn the_shape_half_of_the_message_is_the_real_one() {
        // Guards the report against a fixed string: a file that is a broken *shape* must show the shape
        // parser's complaint, and it is a different sentence from the entity parser's.
        let reason = compile_bytes(
            "in.ron",
            "Union(a: Sphere(r: 1.0, centre: (0.0, 0.0, 0.0)))",
        )
        .expect_err("a Union missing its second child must not compile");
        assert!(
            reason.contains("as a bare shape"),
            "a broken shape must name the shape half: {reason}"
        );
    }
}
