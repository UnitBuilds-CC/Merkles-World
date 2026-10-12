//! Serialising an `Asset` into the `ShapeAsset` envelope `contracts/sdf.fbs` defines.
//!
//! Hand-built rather than generated, because the workspace denies `clippy::all` and forbids `unsafe`,
//! and the `flatbuffers` crate's vector-of-struct support needs `Push`/`Follow`, which are unsafe
//! traits - so a generated module would need a scoped allow that is ADR-031's decision to make, not
//! this file's. The claim that these bytes are a *valid* FlatBuffer is therefore not made here: CI
//! hands them to `flatc`'s own parser and asserts the fields come back (`asset schema (flatc)` job),
//! which is an oracle outside this code.
//!
//! Layout, learned from comparing against `flatc`'s encoding of the same asset rather than from
//! guessing. Every one of these was wrong in a first draft that `flatc` refused to read:
//!
//! - the file is `[root uoffset][file identifier "MLWS"][vtable][table][nodes][params][label]`;
//! - a table begins with a **signed** offset to its vtable, and the relation is
//!   `vtable_pos = table_pos - soffset`, which is why the vtable sits at a *lower* address than the
//!   table it describes;
//! - **a field equal to its schema default is omitted**: `grammar_version` defaults to 1, so its
//!   vtable slot is 0 and nothing is inline for it. Writing it anyway yields a buffer a correct
//!   reader still accepts but that no longer matches what the reference compiler produces;
//! - root and vector uoffsets are relative to the position of the offset field itself, not to the
//!   start of the file.
//!
//! Alignment is 4 throughout: the node struct's fields are four `u32`s, so its alignment is 4, and
//! the schema uses no 8-byte scalars. If that ever stops being true, this file is wrong in a way
//! `flatc`'s reader will catch, which is the point of the CI check.

use crate::Asset;
use contracts::sdf::{NodeRecord, GRAMMAR_VERSION};

/// The schema's `file_identifier`, written after the root offset so a loader can refuse the wrong
/// file in one comparison.
pub const FILE_IDENTIFIER: [u8; 4] = *b"MLWS";

/// Vtable slots for `ShapeAsset`, in declaration order.
const SLOTS: u16 = 5;

fn put_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn node_bytes(record: &NodeRecord) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, word) in record.0.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// Encode an asset. `label` is authoring metadata the interpreter never reads; it survives because
/// hot-reload logs want to name the asset that failed.
pub fn to_bytes(asset: &Asset, label: &str) -> Vec<u8> {
    let label_bytes = label.as_bytes();
    let node_count = asset.tree.nodes.len();
    let param_count = asset.tree.params.len();

    // Positions, computed before anything is written, because FlatBuffers offsets are relative and
    // cross-linked. Lengths first, then the table, then the vtable, then the header.
    let vtable_len = 4 + 2 * usize::from(SLOTS);
    // The table is the soffset plus one slot per field the vtable actually points at. Leaving
    // grammar_version out here while its slot said 0 is what made the first version of this file
    // crash flatc: a vtable may omit a field, but it may not advertise one the table does not hold.
    let grammar_omitted = GRAMMAR_VERSION == 1;
    let inline_fields: usize = if grammar_omitted { 4 } else { 5 };
    // The object size includes the leading soffset, which is what the structural test in
    // tests/written_asset.rs caught: a table_len that counted only the fields advertised a label
    // slot outside the object it declared.
    let table_len = 4 + 4 * inline_fields;
    let nodes_len = 4 + node_count * 16;
    let params_len = 4 + param_count * 4;
    let label_len = 4 + label_bytes.len() + 1;

    let header = 8; // root uoffset and the file identifier
    let vtable_at = header;
    let table_at = vtable_at + vtable_len;
    let nodes_at = table_at + table_len;
    let params_at = nodes_at + nodes_len;
    let label_at = params_at + params_len;

    let mut buf = Vec::with_capacity(label_at + label_len);
    put_u32(&mut buf, table_at as u32); // root: the table, relative to this field at 0
    buf.extend_from_slice(&FILE_IDENTIFIER);

    // Vtable: its own size, the table's size, then one u16 per field - relative to the vtable start,
    // or 0 when the field is absent because it matches its default.
    put_u16(&mut buf, vtable_len as u16);
    put_u16(&mut buf, table_len as u16);
    put_u16(&mut buf, if grammar_omitted { 0 } else { 4 }); // grammar_version
    put_u16(&mut buf, if grammar_omitted { 4 } else { 8 }); // depth
    put_u16(&mut buf, if grammar_omitted { 8 } else { 12 }); // nodes
    put_u16(&mut buf, if grammar_omitted { 12 } else { 16 }); // params
    put_u16(&mut buf, if grammar_omitted { 16 } else { 20 }); // label

    // Table: the signed distance back to the vtable, then each field in the order the vtable slots
    // just declared. A vector or string offset is relative to the position of the offset itself.
    put_u32(&mut buf, (table_at - vtable_at) as u32);
    if !grammar_omitted {
        put_u32(&mut buf, GRAMMAR_VERSION);
    }
    put_u32(&mut buf, asset.depth);
    // A uoffset names the target's own position - the vector's length field, the string's length
    // field - and is measured from the offset itself. Pointing one field further, at the elements,
    // is the off-by-one that made flatc read a garbage length and crash.
    put_u32(&mut buf, (nodes_at - (table_at + 8)) as u32);
    put_u32(&mut buf, (params_at - (table_at + 12)) as u32);
    put_u32(&mut buf, (label_at - (table_at + 16)) as u32);

    // Nodes: a count, then the inline structs - the property that keeps this file the interpreter's
    // buffer, which is why the schema chose structs over tables.
    put_u32(&mut buf, node_count as u32);
    for record in &asset.tree.nodes {
        buf.extend_from_slice(&node_bytes(record));
    }

    put_u32(&mut buf, param_count as u32);
    for value in &asset.tree.params {
        put_u32(&mut buf, value.to_bits());
    }

    put_u32(&mut buf, label_bytes.len() as u32);
    buf.extend_from_slice(label_bytes);
    buf.push(0);
    buf
}
