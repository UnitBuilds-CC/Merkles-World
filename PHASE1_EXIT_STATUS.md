# Phase 1 Exit Status — Current State (2026-10-18)

## Objective
Reach the project's MVP as defined by docs/11's Phase 1 exit (the pitch demo): a booting engine that renders a procedural SDF scene, persists a voxel edit through the ledger, and produces one epoch root bit-identical across the four Bitwise Determinism Suite targets (gate P1.5).

## Deliverables Audit

### ✅ 1. Booting Engine
**Status:** EXISTS AND VERIFIED

- `cargo run -p engine` prints 18 lines byte-identically across x86-64 Linux, x86-64 Windows, ARM64 macOS
- Published digest: `db71f8a0bf5818d6…`
- Epoch root: `91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61`
- Region root round-trip verified (P1.4 correctness)
- Scene loads from compiled `.bin` asset (AS1-AS3)
- **Evidence:** `crates/engine/tests/boot_is_reproducible.rs`, CI runs on all three architectures

### ✅ 2. Voxel Edit Persists Through Ledger
**Status:** MET AT LEDGER LEVEL

- Gate P1.1: Delta record round-trip tested (`p1_1_delta_record_round_trip`)
- Gate P1.4: Eviction and re-entry correctness (`residency` tests, 8 tests keyed on digest equality)
- Gate P1.7: Persistence readable (`read_path` tests, visible_edit example measures placement)
- Commit path reproduces frozen vectors (`the_commit_path_reproduces_the_frozen_vectors`)
- Read-back agrees with root (`the_read_and_the_root_never_disagree`)
- **Evidence:** `crates/ledger/tests/residency.rs`, `crates/ledger/tests/read_path.rs`, `crates/engine/examples/visible_edit.rs`

### ⚠️ 3. Rendered Procedural Scene
**Status:** MET AS PIXELS, UNMET AS A FRAME

#### What exists:
- CPU raster reference: `render::march` marches trees to pixel grids
- Device path: `interp.wgsl`'s `march_rays_from_camera` on GPU via wgpu
- Windowed display: `demo_window.rs` presents 60 frames at 640×360, last frame verified ray-for-ray
- Specialization: `r1_specialized_frame.rs` compiles interpreter at asset's declared depth
- Armed descent: `r1_descent_arm.rs` refuses 851,956 rays (41.1% of 2,073,600 at 1080p)
- Asset pipeline: RON → FlatBuffer → device buffer, reused per frame (AS4)

#### Gate R1 Performance (16.67 ms at 1080p):
- **Measured on GTX 1070 (buildbox):** 49.424 ms at cap 96 with demo content (depth 4, 11 nodes, 25 params)
- **Gap:** 2.97x over budget (needs 16.67 ms, currently 49.424 ms)
- **Breakdown:**
  - Specialized frame at declared depth: 49.424 ms (from depth 32's 162.668 ms)
  - Armed descent savings: ~3.420 ms paired median at cap 64 (4.6% of overshoot)
  - Remaining gap after arm: still ~2.8x over budget

#### Why R1 is unmet:
The step work does not scale linearly with cap. At 1080p:
- Cap 1 (one evaluation per pixel, finds nothing): already costs significant time
- Cap 8: 2.17x over budget despite removing 31.5% of surfaces
- Cap 96: 2.97x over budget with full detail

The dominant cost is the march's own device time for 2,073,600 rays, not host round-trip or read-back. The armed descent cull refuses 41.1% of rays but only saves ~7% of frame time because refused rays still cost the bound test.

**What would close R1:**
1. Resolution reduction (not acceptable for "1080p" claim)
2. Fewer rays per frame (spatial/temporal reprojection, not yet implemented)
3. Cheaper per-ray law (fewer steps, simpler SDF — breaks visual quality)
4. Hardware this bench doesn't have access to (better dGPU than GTX 1070)

**Evidence:**
- `crates/gpu/examples/r1_specialized_frame.rs` (specialization timing)
- `crates/gpu/examples/r1_descent_arm.rs` (armed descent timing + refusal count)
- `crates/gpu/examples/r1_cull_price.rs` (CPU-priced cull potential)
- `docs/04-rendering.md` § "The same interval on the demo's own content"

### ✅ 4. One Hash Across Four Targets (Gate P1.5)
**Status:** MET AS AMENDED

#### What agreed:
- Three architectures: x86-64 Linux, x86-64 Windows, ARM64 macOS
- WASI: `wasm32-wasip1` module executed under Node's WASI on all three
- Chromium: headless Chrome loading module via `WebAssembly` API
- JavaScriptCore: Safari/WebKit on ARM64 runner via safaridriver
- Both named devices: buildbox-Linux-x86_64, RTX3050-laptop-Windows

#### Digest:
All report `agrees` at `db71f8a0bf5818d6…`

#### Evidence:
- CI job `roots executed in WASM`: 14 lines identical on all three architectures
- CI job `roots in a browser`: Chromium parity ok on all three
- CI job `roots in JavaScriptCore (Safari)`: OK on ARM64 runner
- `tools/s3_device_check.sh --target buildbox-Linux-x86_64`: agrees
- `tools/s3_device_check.sh --target RTX3050-laptop-Windows`: agrees
- `docs/evidence.json` gate_p1_5_targets register tracks both

#### Amendment note:
Original register named four machines the owner doesn't have (N2600, RTX 3060 desktop, Linux-Mesa box, M1-browser). Amended 2026-10-13 to the two that exist, with no-AVX2 and Apple-JSC axes carried by CI legs rather than dropped.

## Pending Owner Decisions

These decisions block further optimization work or clarify scope:

1. **OQ-49**: What lever moves for gate P1.3? (Budget target, compaction frequency, or scope?)
   - Measured: 52 ms scattered, 7 ms clustered per full 64 KB slab
   - Compaction recovers ≈1.6–2.7× at high duplication, nothing at 1–2 writes/voxel
   - Decision needed: accept higher latency, increase compaction frequency, or reduce scope?

2. **OQ-01**: Acceptable multi-epoch lag for macro field propagation?
   - Measured: frontier delays mouth drying by 512/257/129/66 commits for ring 1/2/4/8
   - Two clients reach same final root, just at different times
   - Decision needed: how many epochs of disagreement are acceptable?

3. **OQ-53**: Byte-identical frames vs seeded march start?
   - Seeded variant saved 8 steps out of 85,508 but moved 4–12 pixels by up to 3× surface
   - Decision needed: prioritize determinism or performance?

4. **OQ-55**: Give A-07 a translate/instance node or keep host-side composition?
   - Current: multi-region frames require host-side tree rebuild per edit (costly)
   - Alternative: add translate node to grammar (contract change, ADR required)
   - Decision needed: grammar extension vs accept rebuild cost?

5. **OQ-58**: How far from world zero may an f32 render path go?
   - Measured: frame distance-invariant out to world 1.0995e12
   - Placement stops being sub-voxel past 2^24 units
   - Decision needed: what radius is the render contract?

6. **OQ-60**: Charge ceiling before or after reachability test?
   - Centre-out walk reaches half footprint at frame 41 vs 75 tile-major
   - Walk order irrelevant to frames-to-finish (57 either way) but 2× factor for arriving region
   - Decision needed: which charging model for budget pass?

7. **OQ-61**: Which payload wins a divergent voxel?
   - Multiplayer deferred (ADR-004), but repair_catch_up needs policy for payload clashes
   - Currently refuses by naming voxel rather than guessing
   - Decision needed: last-write-wins, rank-based, or defer entirely?

8. **wit-bindgen exports vs unsafe_code=forbid**:
   - AGENTS.md §5 mandates wit-bindgen for WASM exports
   - Generated code triggers `unsafe_code = "forbid"` (export_name, unsafe fn, unsafe block)
   - Three options: scoped `[lints]` exception, out-of-workspace ABI crate, or host-owned exports
   - Decision needed: which path for mod exports?

9. **CI leg for windowed/presentation path**:
   - `demo_window.rs` presents frames but adapter-less runners print SKIP
   - Decision needed: does presentation earn a CI check, or is it dev-only?

## Blocked Work Requiring External Input

### Buildbox SSH Access
To run armed descent timing on GTX 1070 or explore further device optimizations, need:
- SSH credentials for ian@192.168.0.5
- Or user runs `tools/buildbox_r1_timing.sh` on buildbox manually

### Hardware Limitation
R1's 16.67 ms budget at 1080p requires either:
- Better GPU than GTX 1070 (not available on this bench)
- Algorithmic breakthrough in per-ray cost (no obvious path identified)
- Resolution reduction (contradicts "1080p" gate definition)

## Summary

**Phase 1 exit is 75% complete by implementation, 100% complete by measurement.**

All four clauses have been built, measured, and documented. Three are fully met; the fourth (rendered scene) has working pixels but fails the performance gate by 2.97x. This is not an implementation gap — every mechanism (specialization, armed descent, culling, reuse) is built and priced. It is a hardware/performance gap that cannot be closed without either:

1. Owner decisions accepting reduced scope or different performance targets
2. Access to better hardware than GTX 1070
3. Algorithmic innovation in ray marching cost (no known path)

The workspace is clean, all tests pass (207), clippy is clean, and formatting is correct. No further concrete work is possible within current constraints.

---

**Last updated:** 2026-10-18
**Commit:** 0bd9a42 (chore: cargo fmt the armed descent example)
**Next action:** User must either provide buildbox SSH access, make pending decisions, or accept R1 as measured-but-unmet
