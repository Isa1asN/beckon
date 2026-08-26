//! Agent-specific translation, and the only place agent quirks may live.
//!
//! Everything downstream of an adapter sees a [`crate::core::event::Event`], so
//! adding an agent never touches policy, packs or audio.

pub mod claude_code;

pub use claude_code::ClaudeCode;

use crate::core::event::Event;
use crate::settings_json::InstallPlan;
use std::path::{Path, PathBuf};

/// Which settings file to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Applies to every project.
    User,
    /// Applies to this repository only, and can be committed.
    Project,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

pub trait Adapter {
    /// Canonical id, as used by `beckon hook <agent>` and in config.
    fn id(&self) -> &'static str;

    /// Where this agent keeps the settings file for `scope`.
    ///
    /// `None` when the location cannot be determined — no home directory, for
    /// instance — which `init` reports rather than guessing.
    fn settings_path(&self, scope: Scope, project_root: &Path) -> Option<PathBuf>;

    /// The hooks to bind, given the absolute command that invokes beckon.
    fn install_plan(&self, command: &str) -> InstallPlan;

    /// Translate one raw hook payload.
    ///
    /// `None` means the payload was unusable — not that it was uninteresting.
    /// An understood-but-silent event returns
    /// [`crate::core::event::Signal::Ignore`], which keeps "we could not read
    /// this" distinguishable from "nothing to do" in traces.
    fn parse(&self, stdin: &[u8]) -> Option<Event>;
}

/// Agent binaries beckon knows how to look for. Used by `beckon doctor`.
pub const KNOWN_AGENTS: [&str; 1] = ["claude"];

pub fn adapter_for(id: &str) -> Option<Box<dyn Adapter>> {
    match id {
        "claude-code" | "claude" => Some(Box::new(ClaudeCode)),
        _ => None,
    }
}

/// Append a raw payload to `$BECKON_DUMP`, if set.
///
/// Several hook payload schemas are undocumented. This is how a real one gets
/// captured so a fixture can be corrected, and it is the first thing
/// `beckon doctor` suggests when an adapter looks wrong.
pub fn dump_if_requested(stdin: &[u8]) {
    if let Some(path) = std::env::var_os("BECKON_DUMP") {
        dump_to(Path::new(&path), stdin);
    }
}

/// Testable half of [`dump_if_requested`]. Never fails.
pub fn dump_to(path: &Path, stdin: &[u8]) {
    use std::io::Write;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    // One payload per line, so `jq` can read the file directly.
    let mut line: Vec<u8> = stdin.iter().copied().filter(|b| *b != b'\n').collect();
    line.push(b'\n');
    let _ = file.write_all(&line);
}
