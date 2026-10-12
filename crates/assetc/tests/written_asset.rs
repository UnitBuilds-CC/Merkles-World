//! Structural self-consistency of the bytes `writer::to_bytes` produces.
//!
//! These assert what a reader would resolve, not what our own reader likes: the root must point at a
//! table whose signed offset resolves back to a vtable that fits, every advertised field must lie
//! inside the object the vtable declares, and the node vector's length must agree with the buffer's
//! size. That is the class of mistake that made the first version of this writer crash `flatc`
//! rather than merely fail it - a vtable advertising a field the table never wrote.
//!
//! The independent check is not here: CI feeds these same bytes to the pinned `flatc` parser
//! (`asset schema (flatc)` job), which is the only claim about validity that does not come from the
//! code under test.

use assetc::lower;
use assetc::writer::to_bytes;
use assetc::Shape;

fn probe() -> Vec<u8> {
    let asset = lower(&Shape::Sphere {
        r: 1.0,
        centre: [0.0, 0.0, 0.0],
    })
    .expect("a sphere lowers");
    to_bytes(&asset, "probe: a unit sphere at the origin")
}

fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().expect("four bytes"))
}

fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(buf[at..at + 2].try_into().expect("two bytes"))
}

#[test]
fn the_root_resolves_to_a_table_whose_vtable_resolves_back() {
    let buf = probe();
    let table = u32_at(&buf, 0) as usize;
    assert!(
        table + 4 <= buf.len(),
        "the root offset must land inside the buffer"
    );
    // A vtable sits at a lower address, so the offset is subtracted - the sign convention that the
    // first draft of this writer got backwards.
    let soffset = u32_at(&buf, table) as usize;
    assert!(
        soffset > 0 && soffset <= table,
        "a vtable cannot sit before the file"
    );
    let vtable = table - soffset;
    let vt_size = u16_at(&buf, vtable) as usize;
    let obj_size = u16_at(&buf, vtable + 2) as usize;
    assert_eq!(vt_size % 2, 0, "a vtable is an array of u16");
    assert!(
        vtable + vt_size <= table,
        "the vtable must not overrun the table"
    );
    assert!(
        obj_size >= 4 && table + obj_size <= buf.len(),
        "the table must fit the object its vtable declares"
    );

    // Every field the vtable advertises must be inside that object. Slot 0 (grammar_version) is 0
    // here because the value equals the schema default, which is a FlatBuffers behaviour and not a
    // hole.
    for slot in 2..5usize {
        let offset = u16_at(&buf, vtable + 4 + slot * 2) as usize;
        assert!(
            offset >= 4 && offset < obj_size,
            "field slot {slot} escapes the table"
        );
        assert_eq!(offset % 4, 0, "every ShapeAsset field is a 4-byte scalar");
    }
}

#[test]
fn the_node_vector_length_agrees_with_the_buffer_and_the_string_terminates() {
    let buf = probe();
    let table = u32_at(&buf, 0) as usize;
    let soffset = u32_at(&buf, table) as usize;
    let vtable = table - soffset;
    let obj_size = u16_at(&buf, vtable + 2) as usize;

    // Nodes and params are addressed relative to their own offset fields, so resolve them the way a
    // reader must.
    let nodes_at = {
        let slot = u16_at(&buf, vtable + 4 + 2 * 2) as usize;
        let field = table + slot;
        field + u32_at(&buf, field) as usize
    };
    let count = u32_at(&buf, nodes_at) as usize;
    assert_eq!(count, 1);
    assert!(
        nodes_at + 4 + count * 16 <= buf.len(),
        "a node is a 16-byte inline struct, so the vector cannot overrun"
    );

    let label_at = {
        let slot = u16_at(&buf, vtable + 4 + 4 * 2) as usize;
        let field = table + slot;
        field + u32_at(&buf, field) as usize
    };
    let len = u32_at(&buf, label_at) as usize;
    assert_eq!(len, "probe: a unit sphere at the origin".len());
    assert_eq!(
        buf[label_at + 4 + len],
        0,
        "FlatBuffers strings are NUL-terminated so a reader can borrow them directly"
    );
    assert_eq!(
        obj_size % 4,
        0,
        "the table stays 4-aligned, as its fields require"
    );
}
