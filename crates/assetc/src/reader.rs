//! Reading a compiled `ShapeAsset` - the runtime side of doc 09's "load pre-compiled binaries".
//!
//! The bar for this reader is deliberately the opposite direction from the writer's: it must parse
//! bytes that `flatc` produced, not bytes this repo produced. A writer and reader built side by side
//! can agree on a mistake; a reader that walks the reference compiler's own output cannot, which is
//! why `tests/asset_round_trip.rs` feeds this code a file packed by `flatc` and checked in as a
//! fixture rather than a buffer made by `to_bytes`.
//!
//! The layout it walks is the one `writer.rs` documents: root uoffset, `MLWS` identifier, vtable,
//! table, then the inline node structs and parameter pool. Field presence is decided by the vtable,
//! not by position - a field equal to its schema default is absent from the table entirely, so
//! reading positionally would silently return the wrong value rather than fail.
//!
//! What this is not: a general FlatBuffers reader. It understands one table, three field kinds (u32,
//! vector of the 16-byte node struct, vector of f32, string), and it refuses anything else, because a
//! shape loader that guesses at an unfamiliar buffer produces a wrong picture instead of an error.

use contracts::sdf::NodeRecord;

/// The schema's `file_identifier`.
pub const FILE_IDENTIFIER: [u8; 4] = *b"MLWS";

/// Why a buffer is not an asset this engine will run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Too short to hold a header, let alone a table.
    Truncated,
    /// The four bytes after the root offset are not `MLWS`.
    WrongIdentifier,
    /// An offset points outside the buffer, or a vtable claims fields the table does not hold.
    Corrupt,
    /// A vtable slot this reader does not understand (a struct field, an out-of-order table).
    UnsupportedField,
}

/// The decoded asset: the node array, the parameter pool, and the label the compiler stamped in.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAsset {
    pub grammar_version: u32,
    pub depth: u32,
    pub nodes: Vec<NodeRecord>,
    pub params: Vec<f32>,
    pub label: String,
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32, ReadError> {
    let bytes: [u8; 4] = buf
        .get(at..at + 4)
        .ok_or(ReadError::Truncated)?
        .try_into()
        .expect("four");
    Ok(u32::from_le_bytes(bytes))
}

fn u16_at(buf: &[u8], at: usize) -> Result<u16, ReadError> {
    let bytes: [u8; 2] = buf
        .get(at..at + 2)
        .ok_or(ReadError::Truncated)?
        .try_into()
        .expect("two");
    Ok(u16::from_le_bytes(bytes))
}

/// Parse an asset. `Ok` means the offsets all resolve inside the buffer and the fields this schema
/// knows about were found where the vtable said they would be.
pub fn from_bytes(buf: &[u8]) -> Result<CompiledAsset, ReadError> {
    if buf.len() < 16 {
        return Err(ReadError::Truncated);
    }
    let root = u32_at(buf, 0)? as usize;
    if buf[4..8] != FILE_IDENTIFIER {
        return Err(ReadError::WrongIdentifier);
    }
    // A uoffset resolves relative to its own position, and the root offset sits at byte 0 - so the
    // table is at `root`, not `root + 4`. (The four bytes after the root are the file identifier, not
    // a header to step over, and getting this wrong reads the identifier as a soffset.)
    let table = root;
    if table + 4 > buf.len() {
        return Err(ReadError::Corrupt);
    }
    // A table starts with a signed offset backwards to its vtable.
    let soffset = u32_at(buf, table)? as usize;
    if soffset == 0 || soffset > table {
        return Err(ReadError::Corrupt);
    }
    let vtable = table - soffset;
    let vt_size = u16_at(buf, vtable)? as usize;
    let obj_size = u16_at(buf, vtable + 2)? as usize;
    if vtable + vt_size > table || table + obj_size > buf.len() || obj_size < 4 {
        return Err(ReadError::Corrupt);
    }

    let slots = (vt_size - 4) / 2;
    if slots > 5 {
        return Err(ReadError::UnsupportedField);
    }
    let field = |slot: usize| -> Result<Option<usize>, ReadError> {
        if slot >= slots {
            return Ok(None);
        }
        let offset = u16_at(buf, vtable + 4 + slot * 2)? as usize;
        if offset == 0 {
            Ok(None)
        } else if offset >= obj_size {
            Err(ReadError::Corrupt)
        } else {
            Ok(Some(table + offset))
        }
    };

    // Slot 0 is grammar_version, which the schema defaults to 1 and which a writer may therefore omit.
    let grammar_version = match field(0)? {
        Some(at) => u32_at(buf, at)?,
        None => 1,
    };
    let depth = match field(1)? {
        Some(at) => u32_at(buf, at)?,
        None => 0,
    };

    let mut nodes = Vec::new();
    if let Some(at) = field(2)? {
        let target = at
            .checked_add(u32_at(buf, at)? as usize)
            .ok_or(ReadError::Corrupt)?;
        let count = u32_at(buf, target)? as usize;
        if target + 4 + count * 16 > buf.len() {
            return Err(ReadError::Corrupt);
        }
        for i in 0..count {
            let at = target + 4 + i * 16;
            let mut words = [0u32; 4];
            for (w, slot) in words.iter_mut().enumerate() {
                *slot = u32_at(buf, at + w * 4)?;
            }
            nodes.push(NodeRecord(words));
        }
    }

    let mut params = Vec::new();
    if let Some(at) = field(3)? {
        let target = at
            .checked_add(u32_at(buf, at)? as usize)
            .ok_or(ReadError::Corrupt)?;
        let count = u32_at(buf, target)? as usize;
        if target + 4 + count * 4 > buf.len() {
            return Err(ReadError::Corrupt);
        }
        for i in 0..count {
            params.push(f32::from_bits(u32_at(buf, target + 4 + i * 4)?));
        }
    }

    let label = match field(4)? {
        Some(at) => {
            let target = at
                .checked_add(u32_at(buf, at)? as usize)
                .ok_or(ReadError::Corrupt)?;
            let len = u32_at(buf, target)? as usize;
            let text = buf
                .get(target + 4..target + 4 + len)
                .ok_or(ReadError::Corrupt)?;
            String::from_utf8_lossy(text).into_owned()
        }
        None => String::new(),
    };

    Ok(CompiledAsset {
        grammar_version,
        depth,
        nodes,
        params,
        label,
    })
}
