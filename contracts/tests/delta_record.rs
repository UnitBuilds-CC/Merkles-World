//! Gate tests for artifact A-03 (`contracts/delta.rs`). Named for the gate each one *is*, so a CI
//! line is the evidence docs/03 asks for — `test p1_1_delta_record_round_trip ... ok`.

use contracts::coords::{GlobalVoxelPos, LocalOffset, RegionKey};
use contracts::delta::{
    voxel_at, DeltaRecord, EpochHeader, RegionSlot, RegionSlotTable, INTRA_REGION_BITS,
    LOCAL_AXIS_BITS, MAX_REGIONS_PER_EPOCH, PAYLOAD_BITS, RECORD_BITS, REGION_SLOT_BITS,
};
use contracts::errors::LedgerError;

/// `value` in gate P1.1's triple, before the payload field's internals are decided (OQ-45).
const VALUE: u16 = 0xabc;

const fn pos(x: i64, y: i64, z: i64) -> GlobalVoxelPos {
    GlobalVoxelPos { x, y, z }
}

const fn off(x: u16, y: u16, z: u16) -> LocalOffset {
    LocalOffset { x, y, z }
}

fn region(x: i64, y: i64, z: i64) -> RegionKey {
    RegionKey { x, y, z }
}

/// Positions chosen to break floor semantics rather than to be typical: the extremes of the `i64`
/// span, region boundaries ±1, and a deterministic scatter across `±2^40` (an LCG, so the sample is
/// the same on every machine — a clock-seeded rng would make the gate unreproducible).
fn axis_samples() -> Vec<i64> {
    let mut v: Vec<i64> = vec![
        i64::MIN,
        i64::MIN + 1,
        -(1 << 51),
        -(1 << 40),
        -4_194_304,
        -4096,
        -4095,
        -1,
        0,
        1,
        4095,
        4096,
        1 << 40,
        (1 << 51) - 1,
        i64::MAX - 1,
        i64::MAX,
    ];
    let span: i64 = 1 << 40;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..2_000 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let spread = (state >> 12) as i64 % span;
        v.push(if state & 1 == 0 { spread } else { -spread });
    }
    v
}

/// Gate P1.1: every delta encodes and decodes back to the same `(RegionKey, [u16; 3], value)` —
/// over the whole `i64` range, negatives included. The reconstruction goes through a real slot
/// table and a real header, because a record alone cannot be checked: its region half is a lookup.
#[test]
fn p1_1_delta_record_round_trip() {
    let samples = axis_samples();
    let mut buffer = vec![region(0, 0, 0); MAX_REGIONS_PER_EPOCH];
    let mut table = RegionSlotTable::new(&mut buffer);

    for (index, &a) in samples.iter().enumerate() {
        let p = pos(a, a.wrapping_neg(), a.wrapping_mul(3));
        let slot = match table.assign(p.region_key()) {
            Ok(slot) => slot,
            Err(e) => panic!(
                "slot ceiling reached at sample {index} of {}: {e:?}",
                samples.len()
            ),
        };
        let payload = (index as u16) & 0x0fff;
        let record = DeltaRecord::at_position(slot, &p, payload);

        assert_eq!(record.slot(), slot, "slot escaped its field at {p:?}");
        assert_eq!(record.intra_region(), p.local_offset(), "triple at {p:?}");
        assert_eq!(record.payload(), payload, "payload at {p:?}");

        // The gate's own wording, field by field.
        let decoded = (
            table.resolve(record.slot()).unwrap(),
            [
                record.intra_region().x,
                record.intra_region().y,
                record.intra_region().z,
            ],
            record.payload(),
        );
        let wanted = (
            p.region_key(),
            [p.local_offset().x, p.local_offset().y, p.local_offset().z],
            payload,
        );
        assert_eq!(decoded, wanted, "P1.1 triple mismatch at {p:?}");
    }
}

/// Gate P1.1's other half: the record is the *only* thing a replay needs to find the voxel, so
/// `slot → region → origin + offset` must land back on the position that was broken.
///
/// The positions are chosen at region edges rather than scattered, because that is where a
/// reconstruction goes wrong: an off-by-one in the shift or a clamp instead of a mask shows up as
/// the voxel just across a boundary resolving into the neighbouring region.
#[test]
fn p1_1_record_resolves_back_to_the_exact_voxel() {
    let mut positions: Vec<GlobalVoxelPos> = Vec::new();
    let size = GlobalVoxelPos::REGION_SIZE;
    for &anchor in &[
        0i64,
        1,
        -1,
        4_096,
        -4_096,
        1 << 21,
        -(1 << 21),
        1 << 34,
        -(1 << 34),
        i64::MAX,
        i64::MIN,
    ] {
        for edge in [-2i64, -1, 0, 1, 2] {
            let x = anchor.wrapping_add(edge);
            positions.push(pos(x, 0, 0));
            positions.push(pos(0, x, 0));
            positions.push(pos(0, 0, x));
            // One axis at a region boundary while the others sit at the record's own extremes.
            positions.push(pos(x, i64::MAX, i64::MIN));
            positions.push(pos(i64::MIN, x, i64::MAX));
        }
    }
    // Every corner of a region, in the region on both sides of zero.
    for base in [0i64, size, -size, 1 << 34] {
        for &dx in &[0i64, size - 1] {
            for &dy in &[0i64, size - 1] {
                for &dz in &[0i64, size - 1] {
                    positions.push(pos(base + dx, base + dy, base + dz));
                }
            }
        }
    }
    assert!(
        positions.len() >= 200,
        "the sweep is supposed to be broad, it found only {}",
        positions.len()
    );

    let mut buffer = vec![region(0, 0, 0); MAX_REGIONS_PER_EPOCH];
    let mut table = RegionSlotTable::new(&mut buffer);
    for p in positions {
        let slot = table
            .assign(p.region_key())
            .unwrap_or_else(|e| panic!("no slot for {p:?}: {e:?}"));
        let record = DeltaRecord::at_position(slot, &p, VALUE);
        let header = EpochHeader {
            epoch_index: 7,
            base_genesis_root: [0; 32],
            event_log_root: [0; 32],
            region_slots: table.slots(),
            record_count: 0,
        };
        assert_eq!(header.voxel_of(record), Some(p), "voxel_of lost {p:?}");
        assert_eq!(
            voxel_at(&p.region_key(), &p.local_offset()),
            p,
            "the A-01 split must be reversible at {p:?}"
        );
    }
}

/// The layout is not just derived from the consts — it is the literal ADR-029 writes, field by
/// field, most-significant first. Pinned as an exact word the way C2 pins its hash vectors, so a
/// reordered field cannot pass by being internally consistent.
#[test]
fn record_field_order_is_the_literal_adr_029_word() {
    let record = DeltaRecord::new(RegionSlot(1), off(1, 2, 3), 0xabc);
    assert_eq!(record.as_u64(), 0x0001_0010_0200_3abc);
    // The extremes fill the word exactly: 16 + 36 + 12 bits of all-ones is `u64::MAX`, which is
    // only true because the three fields leave no bit unused.
    assert_eq!(
        DeltaRecord::new(RegionSlot::MAX, off(4095, 4095, 4095), 4095).as_u64(),
        u64::MAX
    );
}

/// Every `u64` decodes, and re-encoding what it decoded to reproduces the word. This is what makes
/// a raw slab read total: `crates/memory` can `from_u64` a bump-pointer slot it never wrote and
/// still get a well-formed record, so the only way a read is wrong is a slot the header lacks.
#[test]
fn every_word_is_a_valid_record() {
    let mut words: Vec<u64> = vec![0, u64::MAX];
    for bit in 0..RECORD_BITS {
        words.push(1u64 << bit);
        words.push(!(1u64 << bit));
    }
    let mut state = 0xdead_beef_cafe_1234u64;
    for _ in 0..4_096 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        words.push(state);
    }
    for word in words {
        let record = DeltaRecord::from_u64(word);
        assert_eq!(record.as_u64(), word);
        let rebuilt =
            DeltaRecord::new(record.slot(), record.intra_region(), record.payload()).as_u64();
        assert_eq!(rebuilt, word, "fields do not tile the word at {word:#x}");
    }
}

/// SPEC-01 composes with the record: a value wider than its field **rolls**, it is never clamped.
/// Clamping would let out-of-range input masquerade as a legal edge of the range, which is the
/// failure mode docs/02 calls out as writing to the wrong region.
#[test]
fn out_of_range_fields_roll_rather_than_clamp() {
    let record = DeltaRecord::new(RegionSlot(1), off(4096, 8191, 65535), 65535);
    assert_eq!(record.intra_region(), off(0, 4095, 4095));
    assert_eq!(record.payload(), 4095);
    assert_eq!(
        record.slot(),
        RegionSlot(1),
        "masking one field must not touch another"
    );

    // And the roll is the same one A-01 makes, so `at_position` cannot produce a record that
    // disagrees with the position it came from.
    let p = pos(-1, -1, -1);
    let slot = RegionSlot(0);
    let via_record = DeltaRecord::at_position(slot, &p, VALUE);
    let via_parts = DeltaRecord::new(slot, p.local_offset(), VALUE);
    assert_eq!(via_record.as_u64(), via_parts.as_u64());
    assert_eq!(via_parts.intra_region(), off(4095, 4095, 4095));
}

/// First-touch order is the replay rule (docs/02): a slot is handed out the first time an epoch
/// touches a region and reused thereafter, so replaying the same sequence rebuilds the same table.
#[test]
fn slot_table_assigns_in_first_touch_order() {
    let mut buffer = [region(0, 0, 0); 8];
    let mut table = RegionSlotTable::new(&mut buffer);
    let a = region(-3, 7, 0);
    let b = region(0, 0, 0);
    let c = region(1, 2, 3);

    assert_eq!(table.assign(a), Ok(RegionSlot(0)));
    assert_eq!(table.assign(b), Ok(RegionSlot(1)));
    assert_eq!(
        table.assign(a),
        Ok(RegionSlot(0)),
        "a repeat touch reuses its slot"
    );
    assert_eq!(table.assign(c), Ok(RegionSlot(2)));
    assert_eq!(table.count(), 3);
    assert_eq!(table.find(b), Some(RegionSlot(1)));
    assert_eq!(table.resolve(RegionSlot(1)), Some(b));
    assert_eq!(table.find(region(9, 9, 9)), None);
    assert_eq!(
        table.slots(),
        &[a, b, c],
        "the header's table is the assignment sequence, in order"
    );
}

/// The ceiling is checked, not assumed (docs/03 calls it a hard limit). `Err` rather than a
/// corrupted slot, because the record field cannot address a 65,537th region and a silent wrap
/// would point a record at the wrong region — a different world, not a crash.
#[test]
fn slot_table_reports_the_limit_instead_of_wrapping() {
    let mut buffer = [region(0, 0, 0); 4];
    let mut table = RegionSlotTable::new(&mut buffer);
    for i in 0..4 {
        assert_eq!(table.assign(region(i, 0, 0)), Ok(RegionSlot(i as u16)));
    }
    assert!(table.is_full());
    assert_eq!(
        table.assign(region(99, 0, 0)),
        Err(LedgerError::EpochRegionSlotsExhausted)
    );
    assert_eq!(table.count(), 4, "a refused assign changes nothing");
    assert_eq!(table.resolve(RegionSlot(4)), None);

    // The ceiling that matters is the field's, not the buffer's: a larger buffer cannot address
    // past 2^16 regions, and `MAX_REGIONS_PER_EPOCH` is exactly that.
    assert_eq!(MAX_REGIONS_PER_EPOCH, 1 << REGION_SLOT_BITS);
    assert_eq!(MAX_REGIONS_PER_EPOCH, u16::MAX as usize + 1);
    assert_eq!(RegionSlot::MAX.0 as usize + 1, MAX_REGIONS_PER_EPOCH);

    // A buffer wider than the field still stops at the field.
    let mut wide = vec![region(0, 0, 0); MAX_REGIONS_PER_EPOCH];
    let mut big = RegionSlotTable::new(&mut wide);
    assert_eq!(big.capacity(), MAX_REGIONS_PER_EPOCH);
    for i in 0..4_096 {
        assert!(big.assign(region(i as i64, 0, 0)).is_ok());
    }
    assert!(!big.is_full(), "4096 of 65536 slots");
}

#[test]
fn record_bit_fields_tile_one_word() {
    assert_eq!(
        REGION_SLOT_BITS + INTRA_REGION_BITS + PAYLOAD_BITS,
        RECORD_BITS
    );
    assert_eq!(LOCAL_AXIS_BITS * 3, INTRA_REGION_BITS);
    assert_eq!(
        LOCAL_AXIS_BITS,
        GlobalVoxelPos::REGION_SHIFT,
        "the 36 holds only while a region is 4096³ (ADR-029's re-open trigger)"
    );
}

/// Maintainability guard, not a design gate: docs/03 § The delta record contract mirrors this
/// file, and a spec that quietly diverges from the contract is the defect AGENTS.md §8 calls a
/// broken build. Comments may drift; code may not.
#[test]
fn delta_contract_matches_spec_document() {
    let doc = include_str!("../../docs/03-state-ledger.md");
    let contract = include_str!("../../contracts/delta.rs");
    let block = doc
        .split("```rust")
        .find(|b| b.contains("pub struct DeltaRecord"))
        .and_then(|b| b.split("```").next())
        .unwrap_or_else(|| panic!("docs/03 must keep a ```rust block holding the A-03 contract"));
    assert_eq!(
        code_only(block),
        code_only(contract),
        "contracts/delta.rs and docs/03 § The delta record contract have diverged"
    );
}

/// The other half of the same guard, one step further: docs/02 states ADR-029's layout in prose,
/// and prose numbers are the ones a reader copies into code. If the doc's line stops agreeing with
/// the consts, either the doc or the record is wrong and a root is about to fork.
#[test]
fn docs02_layout_line_agrees_with_the_record_consts() {
    let doc = include_str!("../../docs/02-coordinate-system.md");
    let line = doc
        .lines()
        .find(|l| {
            l.contains("region-slot")
                && l.contains("intra-region")
                && l.contains("payload")
                && l.contains("= u64")
        })
        .expect("docs/02 must keep the ADR-029 layout line");
    // region-slot 16 | intra-region 36 (x:12, y:12, z:12) | payload 12, and the `= u64` the line
    // closes with — seven numbers, because the line states the total as well as the parts.
    assert_eq!(
        digit_runs(line),
        vec![
            REGION_SLOT_BITS as usize,
            INTRA_REGION_BITS as usize,
            LOCAL_AXIS_BITS as usize,
            LOCAL_AXIS_BITS as usize,
            LOCAL_AXIS_BITS as usize,
            PAYLOAD_BITS as usize,
            RECORD_BITS as usize,
        ],
        "docs/02's layout line no longer describes the record: {line}"
    );
}

fn digit_runs(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut run: Option<usize> = None;
    for c in text.chars() {
        match c.to_digit(10) {
            Some(digit) => {
                run = Some(run.map_or(digit as usize, |v| v * 10 + digit as usize));
            }
            None => {
                if let Some(value) = run.take() {
                    out.push(value);
                }
            }
        }
    }
    if let Some(value) = run {
        out.push(value);
    }
    out
}

/// Strips the lines a spec block is allowed to differ on. The rule is exactly the coords mirror's:
/// comments (`//`, `///`, `//!`) and blanks may drift, code may not.
fn code_only(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .map(str::to_owned)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect()
}
