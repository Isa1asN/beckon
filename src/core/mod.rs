//! Agent-agnostic core: the vocabulary, configuration, state and gating rules.
//!
//! Nothing in here knows what an oscillator is, and only `state` touches disk.

pub mod config;
pub mod config_edit;
pub mod event;
pub mod identity;
pub mod paths;
pub mod policy;
pub mod state;
