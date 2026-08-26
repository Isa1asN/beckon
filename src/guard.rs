//! The exit-0 guarantee.
//!
//! beckon binds hook events that *block the agent* when a hook exits non-zero
//! (`Stop`, `UserPromptSubmit`, `PreToolUse`). It also binds events whose plain
//! stdout is injected into the model's context (`UserPromptSubmit`,
//! `SessionStart`). Two consequences, and they are not negotiable:
//!
//! 1. Every path out of the process returns 0 — including panics.
//! 2. Nothing reaches stdout unless it is a single well-formed JSON object.
//!
//! A sound tool that can wedge someone's session, or silently poison a prompt,
//! is worse than no sound tool at all.

use std::io::Write;

/// Replace the default panic handler with one that exits 0.
///
/// Install this as the very first statement in `main`, before any work.
///
/// The default handler prints a backtrace to stderr and aborts with a non-zero
/// status. Ours stays quiet unless `BECKON_DEBUG` is set, so a bug in beckon
/// degrades to silence rather than noise in the user's terminal — while still
/// being diagnosable on demand.
pub fn install_panic_guard() {
    std::panic::set_hook(Box::new(|info| {
        if std::env::var_os("BECKON_DEBUG").is_some() {
            let _ = writeln!(std::io::stderr(), "beckon: internal error: {info}");
        }
        exit_ok();
    }));
}

/// Flush and terminate successfully. The only sanctioned way out of a hook.
pub fn exit_ok() -> ! {
    exit_with(0)
}

/// Flush and terminate with a status.
///
/// Only interactive subcommands may pass anything but 0. `hook` and `__play`
/// are invoked by the agent, and a non-zero exit from those can block it.
pub fn exit_with(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code);
}
