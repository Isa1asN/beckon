//! Sound packs: what a personality is made of.
//!
//! A pack is a TOML manifest and nothing else executable. Two source types per
//! sound: a `synth` recipe (about a kilobyte, reviewable as a text diff) or a
//! `sample` file reference.

pub mod builtin;
pub mod manifest;
pub mod resolve;
pub mod store;
