//! `beckon hook <agent>` — the hot path, such as it is.
//!
//! Read the payload, normalize it, decide, and hand playback to a detached
//! child so the agent is never waiting on audio. Every branch returns rather
//! than propagating an error: the caller's only exit code is 0.

use crate::adapter::{adapter_for, dump_if_requested};
use crate::core::config::Config;
use crate::core::event::{Signal, State};
use crate::core::identity;
use crate::core::paths::{self, Paths};
use crate::core::policy::{decide, Decision, PolicyInput};
use crate::core::state;
use crate::trace::trace;
use chrono::{Local, Utc};
use std::io::Read;

/// How long an abandoned session's state is kept before collection.
const SESSION_RETENTION_DAYS: i64 = 7;

pub fn run(agent: &str) {
    let mut payload = Vec::new();
    // Drain stdin even if we end up doing nothing: leaving it unread can hand
    // the agent a broken pipe on the write side.
    if std::io::stdin().read_to_end(&mut payload).is_err() {
        return;
    }
    dump_if_requested(&payload);

    let Some(adapter) = adapter_for(agent) else {
        trace(&format!("ignore unknown-agent {agent}"));
        return;
    };
    let Some(event) = adapter.parse(&payload) else {
        trace("ignore unparseable");
        return;
    };

    let paths = Paths::resolve();
    let now = Utc::now();

    let state = match event.signal {
        Signal::TurnStart => {
            state::record_turn_start(&paths, &event.session_id, now);
            state::prune_older_than(&paths, now, SESSION_RETENTION_DAYS);
            trace("turn-start");
            return;
        }
        Signal::SessionEnd => {
            state::prune_session(&paths, &event.session_id);
            trace("session-end");
            return;
        }
        Signal::Ignore => {
            trace("ignore");
            return;
        }
        Signal::Sound(state) => state,
    };

    // Walk up to the project root: agents are routinely launched from a
    // subdirectory, and a `.beckon.toml` at the repository root must apply.
    let project_root = paths::project_root(&event.project);
    let loaded = Config::load_verbose(&paths, Some(&project_root));
    for warning in &loaded.warnings {
        // Traced rather than printed: stderr here is someone's agent session.
        trace(&format!("config-warning {warning}"));
    }

    // Hold the session lock across read-decide-record, so a burst of events
    // arriving together collapses instead of all reading the same empty
    // history. Failing to get it is not fatal — we simply risk an extra sound.
    let _guard = state::lock_session(&paths, &event.session_id);

    let decision = decide(PolicyInput {
        state,
        config: &loaded.config,
        now: Local::now(),
        muted_until: state::read_mute(&paths),
        // Keyed per session and per state: a different agent, or a different
        // sound, always carries information worth hearing.
        last_played: state::read_last_played(&paths, &event.session_id, state),
        turn_started: state::read_turn_start(&paths, &event.session_id),
    });

    match decision {
        Decision::Suppress(reason) => trace(&format!("suppress {reason}")),
        Decision::Play { state, volume } => {
            state::record_played(&paths, &event.session_id, state, now);
            trace(&format!("play {state}"));
            play(&loaded.config, &project_root, state, volume);
        }
    }
}

/// Hand playback to a detached child.
///
/// The agent waits for this process to exit, so the sound must outlive us. A
/// detached child keeps the hook in single-digit milliseconds no matter how
/// long the sound is.
fn play(config: &Config, project_root: &std::path::Path, state: State, volume: f32) {
    let Ok(exe) = std::env::current_exe() else {
        trace("sound skipped: cannot locate own executable");
        return;
    };
    let transpose = identity::transpose_for(project_root, config.identity.per_project);

    let mut command = std::process::Command::new(exe);
    command
        .arg("__play")
        .arg("--pack")
        .arg(&config.pack)
        .arg("--state")
        .arg(state.as_str())
        .arg("--volume")
        .arg(volume.to_string())
        .arg("--transpose")
        .arg(transpose.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);

    if command.spawn().is_err() {
        trace("sound skipped: could not spawn player");
    }
}

/// Put the child in its own process group so it survives us and is never
/// reaped by, or attributed to, the agent's job control.
#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(DETACHED_PROCESS);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut std::process::Command) {}
