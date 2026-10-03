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

/// Replace the default panic handler.
///
/// Install this as the very first statement in `main`, before any work.
///
/// For the agent's invocations (`agent` is true) it exits 0, staying quiet
/// unless `BECKON_DEBUG` is set, so a bug in beckon degrades to silence rather
/// than a blocked agent or noise in its terminal.
///
/// For a person at a terminal it says so and exits non-zero. Exiting 0
/// silently there was a lie: `beckon config set` on a sample that tripped a
/// decoder bug printed nothing, wrote nothing, and reported success.
pub fn install_panic_guard(agent: bool) {
    std::panic::set_hook(Box::new(move |info| {
        if agent {
            if std::env::var_os("BECKON_DEBUG").is_some() {
                let _ = writeln!(
                    std::io::stderr(),
                    "beckon: internal error: {}",
                    crate::text::safe(&info.to_string())
                );
            }
            exit_ok();
        }
        let _ = writeln!(
            std::io::stderr(),
            "beckon: internal error — this is a bug, and nothing more was done: {}",
            crate::text::safe(&info.to_string())
        );
        exit_with(101);
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
