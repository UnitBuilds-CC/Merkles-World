//! The shared surface between subsystems. Per `AGENTS.md` §2 this is the **only** place agents
//! meet: a subsystem crate may not reach into another crate's internals, and only the Architect
//! role edits these files (a change needs a migration note, a version bump and an ADR).
//!
//! `#![no_std]` is a structural guard for the rules in `AGENTS.md` §3/§4: nothing here can touch
//! the filesystem, the clock, a process, or `alloc`, so no contract type can accidentally become
//! non-reproducible. It holds under `cargo test` too — the contract tests are allocation-free by
//! construction — and `cargo build --target wasm32-unknown-unknown` in CI is what proves it still
//! bites.
#![no_std]

#[path = "../coords.rs"]
pub mod coords;

#[path = "../errors.rs"]
pub mod errors;

/// A-03 — the packed delta record, the epoch's region-slot table, the epoch header (docs/03).
#[path = "../delta.rs"]
pub mod delta;

/// A-08 — the `ActionTx` envelope (docs/10, ADR-021).
#[path = "../tx.rs"]
pub mod tx;

/// A-10 — the Merkle tree shape: node kinds, arity, child order, empty rules, canonical byte order
/// (docs/03 § The tree shape, ADR-030). The grammar is shared; the hasher is `crates/ledger`.
#[path = "../tree.rs"]
pub mod tree;
