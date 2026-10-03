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
use crate::remote;
use crate::trace::trace;
use chrono::{Local, Utc};
use std::io::Read;

/// Largest hook payload beckon will parse. Claude Code's are a few KB; a
/// prompt-sized one is still far below this.
const MAX_PAYLOAD: u64 = 4 * 1024 * 1024;

/// How long an abandoned session's state is kept before collection.
const SESSION_RETENTION_DAYS: i64 = 7;

pub fn run(agent: &str) {
    let mut payload = Vec::new();
    // Bounded: real payloads are a few KB, and an unbounded read into a parsed
    // JSON tree could exhaust memory inside a process the agent waits on.
    // Drain the rest even then: leaving stdin unread can hand the agent a
    // broken pipe on the write side.
    let mut stdin = std::io::stdin().lock();
    if (&mut stdin)
        .take(MAX_PAYLOAD + 1)
        .read_to_end(&mut payload)
        .is_err()
    {
        return;
    }
    if payload.len() as u64 > MAX_PAYLOAD {
        let _ = std::io::copy(&mut stdin, &mut std::io::sink());
        trace("ignore oversized payload");
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
        Signal::Wakeup => {
            // Deliberately leaves the turn timer alone: see `Signal::Wakeup`.
            trace("wakeup");
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
            let config = &loaded.config;
            let route = remote::route(config.remote.mode, remote::ssh_detected());
            // The terminal route can still be closed for this one alert.
            let terminal_closed = if !route.terminal() {
                None
            } else if event.stdout_reaches_model {
                // Such stdout is read as JSON first, but "first" is a parsing
                // detail of one agent version. Not worth a prompt.
                Some("this event's output reaches the model")
            } else if config.remote.sequences.is_empty() {
                Some("remote.sequences is empty")
            } else {
                None
            };
            let to_terminal = route.terminal() && terminal_closed.is_none();

            if !route.local_audio() && !to_terminal {
                // Reached nobody, so it is not recorded as played: a sound you
                // never heard must not rate-limit or gate the next one.
                let why = terminal_closed.unwrap_or("no route");
                trace(&format!("dropped {state}: {why}, and no local audio here"));
                return;
            }

            state::record_played(&paths, &event.session_id, state, now);
            trace(&format!("play {state}"));
            if route.local_audio() {
                play(config, &project_root, state, volume);
            } else {
                trace("local audio skipped: remote");
            }
            if let Some(why) = terminal_closed {
                trace(&format!("terminal skipped: {why}"));
            } else if to_terminal {
                send_to_terminal(config, &project_root, state);
            }
        }
    }
}

/// Hand the agent an escape sequence to write to your terminal.
///
/// The only thing this hook ever prints. It is the agent's JSON hook output,
/// so it must be one complete document or nothing — a partial one is read as
/// a malformed reply, not ignored.
fn send_to_terminal(config: &Config, project_root: &std::path::Path, state: State) {
    let project = remote::project_label(project_root);
    let Some(output) = remote::hook_output(&config.remote.sequences, state, &project) else {
        return;
    };
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush())
        .is_ok()
    {
        trace(&format!("terminal {state}"));
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
