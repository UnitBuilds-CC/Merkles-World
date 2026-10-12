# 08 — Logic & modding (WASM)

**Status:** locked in shape; host API and several operational policies undecided.
**Depends on:** docs 02, 03, 06. **Implements:** ADR-013.

## Why WASM

Not as a scripting convenience — as the thing that makes the ledger's guarantees survivable:

- **Determinism.** WASM pins floating-point to strict IEEE-754 semantics and isolates memory, so
  the same transaction compiled by a modder produces bit-identical results on x86 Windows, an ARM
  MacBook, a Linux server, or a browser via WebGPU. For a Merkle ledger a desync breaks the hash
  chain, so this is load-bearing, not a nicety.
- **Polyglot.** Anything that emits `wasm32` works: Rust (`wasm32-unknown-unknown`), C/C++
  (`clang --target=wasm32`), C# (NativeAOT), Zig, Swift, Go (TinyGo), Python (componentize-py),
  TypeScript (AssemblyScript/Javy). The host sees only the WIT contract, never source.
- **Safety.** A null deref or panic in a mod traps inside the module. The host catches it,
  unloads the module, resets its linear memory buffer — no filesystem access, no arbitrary code
  execution, no GPU pipeline crash, no corrupted epoch root.
- **Speed.** ~80–90% of native.
- **Cross-platform parity with the engine itself.** The same `.wasm` runs in the native and
  browser clients.

**Rejected:** Lua/JS embedding (one language, and GC makes its arithmetic non-reproducible);
native DLL plugins (unsafe and platform-locked); Java-style bytecode injection (there is no
bytecode to inject, and it forfeits the sandbox); C++ headers as the mod surface (kills
portability).

## Host API surface (v1, frozen — proposal)

The whole surface a mod can reach. It lives in `contracts/` and changing it requires an ADR plus
a version bump.

```wit
// contracts/ledger-api.wit  (excerpt)
package game:modding@0.1.0;

interface ledger-api {
  /// Read committed macro state. No chunk loading, no disk access.
  get-region-macro-state: func(region: region-key) -> option<macro-state>;
  get-field: func(region: region-key, field: field-id) -> option<s64>;

  /// Read the player's own pending edits (this client's M_active).
  get-delta: func(pos: global-voxel-pos) -> option<u64>;

  /// Submit a mutation. Never applied synchronously.
  emit-transaction: func(kind: action-kind, region: region-key, payload: list<u8>) -> tx-id;
  emit-event-tag: func(event: event-tag);
}

interface query-api {
  find-regions: func(predicate: list<u8>) -> list<region-key>;   // serialized filter, capped
  count-bytes-read: func() -> u64;                               // quota introspection
}

world mod-plugin {
  import ledger-api;
  import query-api;

  /// Called on the client when the mod is loaded.
  export on-load: func(manifest: list<u8>);

  /// Deterministic per-action handler. May only *propose* transactions.
  export on-action: func(actor: entity-id, kind: action-kind, pos: global-voxel-pos,
                         tick: u64) -> list<proposed-tx>;

  /// Server/authority only: validate a proposed transaction. Rejecting costs the
  /// proposer a fee measured in the in-world resource the exploit would gain.
  export validate-tx: func(tx: proposed-tx) -> result;

  /// Epoch hooks run on the authority during compaction (doc 05 stage 3).
  export on-epoch-start: func(current-epoch: u64);
  export on-epoch-commit: func(current-epoch: u64, dt: u64) -> list<event-tag>;

  /// Circuit node evaluation (doc 06), for mod-defined gate types.
  export evaluate-circuit-node: func(inputs: list<bool>) -> list<bool>;

  /// Macro-field update contribution for one region (doc 05 rule library).
  export step-macro-field: func(region: region-key, field: field-id, dt: u64,
                                 current: s64) -> s64;
}
```

**This block is now an excerpt of a checked file, and being an excerpt it had slipped.** Completing A-02
as `contracts/ledger-api.wit` added a parse test (`contracts/tests/wit_surface.rs`), which found that
WIT spells signed 64-bit integers `s64` - the `i64` above is corrected, and the types are declared
inside `interface ledger-api`, because package scope admits none. Read the file as the surface and this
block as its summary; the test fails if the two drift.

The two lines that carry the architecture: **mutations are proposals, not side effects**
(`on-action` returns transactions; the ledger commits them, and only the ledger emits
`on-epoch-commit`), and **`validate-tx` is where anti-exploit policy lives** — a mod's
consequence-evaluation code is the attack surface, so validation runs on the authority and is
priced, not merely checked ([OQ-31](13-risks-open-questions.md)).

## Determinism rules for mod authors

Enforced by linting at compile-on-save, not at runtime:

1. No wall-clock or system time — use `tick` / `current-epoch` parameters.
2. No unseeded randomness — request a seeded deterministic RNG stream from the host.
3. No transcendental math; integer or fixed-point arithmetic only (values that reach the ledger
   must be bit-identical — [ADR-017](decisions.md)).
4. No ambient state: identical inputs to `on-*` must yield identical outputs, on every platform,
   every time.
5. No assumptions about ordering of other mods' handlers ([OQ-17](13-risks-open-questions.md)
   will define registration order; until then, treat it as undefined).

## Example mod (Rust → WASM)

```rust
// Types below are generated by `wit-bindgen` from `world mod-plugin` (the WIT above);
// the import path is illustrative — the real one is whatever the bindings generate at P1.
use bindings::types::*;

#[export_name = "on-epoch-commit"]
pub fn on_epoch_commit(current_epoch: u64, _dt: u64) -> Vec<EventTag> {
    // Full moon every 28 epochs: propose a wolf-pack migration.
    if current_epoch % 28 == 0 {
        vec![EventTag {
            region: SectorKey { x: 4, y: 0, z: 0 },   // 3×i64, per contracts/coords.rs
            kind: EventKind::WolfPackMigration,
            magnitude: 100,
        }]
    } else {
        vec![]
    }
}
```

`EventTag`'s field list is *illustrative* — the registry is [OQ-08](13-risks-open-questions.md) and
artifact A-05. Do not treat `magnitude: u32` as contract.

Generated with `wit-bindgen` (so the ABI is compiler-checked rather than hand-rolled
`#[no_mangle]`), compiled `cargo build --target wasm32-unknown-unknown`, dropped in `mods/`.

## Mod structure

```
mods/elemental_dragons/
├── mod.ron              # manifest: id, version, engine range, contract versions, deps
├── assets/              # RON entity/biome/item/circuit definitions (→ compiled .bin)
│   └── fire_dragon.ron
├── shaders/             # optional hand-written WGSL (→ validated)
└── logic/               # .wasm modules + their WIT version
```

Mods mutate the world by **emitting ledger transactions** (`RegisterMacroEvent`,
`ApplyLocalDelta`), never by writing block arrays. Two mods editing the same region layer as a
deterministic transaction sequence in the delta mask instead of silently clobbering each other —
the mod-conflict class that plagues Minecraft and Skyrim simply doesn't arise.

## Polyglot support matrix (verify each before publishing as "supported")

| Language | Toolchain | Status |
|----------|-----------|--------|
| Rust | `wasm32-unknown-unknown` + `wit-bindgen` | **primary**, reference implementation |
| TypeScript | AssemblyScript / Javy | planned |
| Python | componentize-py | planned |
| Go | TinyGo | planned |
| C/C++ | `clang --target=wasm32` | planned |
| C# | NativeAOT | experimental |
| Zig / Swift | native wasm targets | experimental |

## Hot reload & lifecycle

The engine watches `mods/`; a saved RON or WGSL file revalidates and recompiles in milliseconds
and the GPU picks up the new node tree without a restart. `.wasm` swap requires an instance
recycle (module compile + instantiate), which is why logic reloads are batched at a safe point
(tick boundary) rather than mid-frame.

**Lifecycle gaps to close:** unload semantics and state cleanup ([OQ-32](13-risks-open-questions.md)),
migration between mod versions ([OQ-33](13-risks-open-questions.md)), and the exact per-module
error surface.

## Limits, trust & distribution

| Concern | Decision | Status |
|---------|----------|--------|
| Execution limits | fuel/`max_instructions` per call, memory cap per module, crash → disable + notify + offer rollback | **OQ-20** — without fuel metering a WASM infinite loop freezes the client; and the budget changes with circuit complexity ([OQ-02](13-risks-open-questions.md)) |
| Permissions | manifest-declared (`query-api`, `emit-event-tag`, …) with host-enforced quotas — byte caps per query, `MAX_FILTER_BYTES`, per-tick result caps (a mod enumerating world state is a memory-exhaustion vector) | **OQ-34** — needs an escalation story: what happens when legitimate content hits a cap, and who tunes it |
| Signing | mods signed; hash recorded in the epoch root so an unsigned swap is detectable by peers | **OQ-11** |
| Version compat | engine declares a minimum contract version; load-time compatibility check | **OQ-33** |
| Sandboxed-but-sneaky | mods that spawn unbounded entities to force expensive field evaluation — the class of exploit this architecture is uniquely open to | **OQ-31** |

**`prompt-to-mod`.** Because everything is text — node trees, RON schemas, WGSL functions, WASM
modules — an in-game LLM can produce a mod from a plain-English request ("a wand that fires green
plasma and crystallizes nearby rock"). The pipeline is: generate schema → generate shader →
compile small WASM → hot-reload. It is a **client-side authoring tool**, not a runtime authority:
anything the model produces must still pass the same validation, sandboxing and signing path as a
hand-written mod ([OQ-21](13-risks-open-questions.md)).

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| W1 | Same transaction sequence → identical epoch root from Rust, TS and Python compiled mods | design claim |
| W2 | A deliberately panicking mod is unloaded with the game still running | sandbox |
| W3 | A mod with an infinite loop is interrupted by the fuel limit within one tick | **OQ-20** |
| W4 | RON edit → visible change in-game without restart | ADR-014 |
| W5 | Two mods editing the same region produce a deterministic, replayable result | design claim |
