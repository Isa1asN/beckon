//! Agent-agnostic core: the vocabulary, configuration, state and gating rules.
//!
//! Nothing in here knows what an oscillator is. Disk access goes through `files`.

pub mod config;
pub mod config_edit;
pub mod event;
pub mod files;
pub mod identity;
pub mod paths;
pub mod policy;
pub mod state;
