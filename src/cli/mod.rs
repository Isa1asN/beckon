//! One module per subcommand.

pub use crate::text::{safe, safe_path};

pub mod config_cmd;
pub mod doctor;
pub mod hook;
pub mod install;
pub mod mute;
pub mod packs;
pub mod play;
pub mod test_cmd;
