//! beckon — give your AI coding agent a voice.
//!
//! The library half exists so integration tests can exercise internals
//! directly; `src/main.rs` is a thin shell over it.

#![forbid(unsafe_code)]

pub mod adapter;
pub mod audio;
pub mod cli;
pub mod core;
pub mod guard;
pub mod pack;
pub mod settings_json;
pub mod trace;
