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
use crate::{lower, Shape};

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
        let text = match std::fs::read_to_string(input) {
            Ok(text) => text,
            Err(error) => return Outcome::Failed(format!("cannot read: {error}")),
        };
        let stamp = fingerprint(&text);
        if self.seen == Some(stamp) {
            return Outcome::Unchanged;
        }
        // Ordered deliberately: nothing below may touch the output file before this returns Ok.
        let bytes = match compile_bytes(input, &text) {
            Ok(bytes) => bytes,
            Err(reason) => return Outcome::Failed(reason),
        };
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
fn compile_bytes(input: &str, text: &str) -> Result<Vec<u8>, String> {
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
        Err(error) => return Err(format!("refused by the compiler: {error:?}")),
    };
    let label = std::path::Path::new(input)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "asset".to_string());
    Ok(to_bytes(&asset, &label))
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
