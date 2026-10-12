//! The device-free half of the render workstream: does A-07's WGSL master interpreter compile?
//!
//! See [`naga_validate`] for why this is checkable without a GPU and what it therefore does not prove.

pub mod naga_validate;
