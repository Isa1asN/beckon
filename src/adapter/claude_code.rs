//! Claude Code.
//!
//! The richest hook surface of any agent: ~30 lifecycle events, with failure
//! and permission states already separated for us. Two consequences shape this
//! adapter.
//!
//! **We never bind the success path.** `PostToolUseFailure` exists as its own
//! event, so beckon adds zero latency to normal tool calls and still hears
//! failures.
//!
//! **`Notification` sub-types itself.** `permission_prompt` versus `idle_prompt`
//! versus `agent_needs_input` is handed to us, so the "needs a decision versus
//! merely waiting" distinction is read, not inferred.

use super::{Adapter, Scope};
use crate::core::event::{Event, Signal, State};
use crate::settings_json::{Binding, InstallPlan};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Every event that maps to a state in the table below, plus the two silent
/// ones. Events whose state is off by default are still bound, so enabling one
/// later is a config edit rather than a re-install.
const BOUND_EVENTS: [&str; 9] = [
    "UserPromptSubmit",
    "Stop",
    "Notification",
    "StopFailure",
    "PostToolUseFailure",
    "SubagentStop",
    "PreCompact",
    "SessionStart",
    "SessionEnd",
];

/// Seconds. Generous — beckon exits in single-digit milliseconds — but a
/// timeout that never fires is the same as no timeout at all.
const HOOK_TIMEOUT: u64 = 5;

pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn settings_path(&self, scope: Scope, project_root: &Path) -> Option<PathBuf> {
        let dir = match scope {
            // Claude Code's own override, so honouring it keeps beckon pointed
            // at the same file the agent actually reads.
            Scope::User => match std::env::var_os("CLAUDE_CONFIG_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => directories::BaseDirs::new()?.home_dir().join(".claude"),
            },
            Scope::Project => project_root.join(".claude"),
        };
        Some(dir.join("settings.json"))
    }

    fn install_plan(&self, command: &str) -> InstallPlan {
        InstallPlan {
            bindings: BOUND_EVENTS
                .iter()
                .map(|event| Binding {
                    event,
                    command: command.to_string(),
                    timeout: HOOK_TIMEOUT,
                })
                .collect(),
        }
    }

    fn parse(&self, stdin: &[u8]) -> Option<Event> {
        let value: Value = serde_json::from_slice(stdin).ok()?;
        let obj = value.as_object()?;
        let hook = string_at(obj, "hook_event_name")?;

        let signal = match hook {
            "Stop" => Signal::Sound(State::Done),

            "Notification" => match string_at(obj, "notification_type") {
                Some("permission_prompt" | "agent_needs_input" | "elicitation_dialog") => {
                    Signal::Sound(State::NeedsYou)
                }
                Some("idle_prompt") => Signal::Sound(State::IdleWaiting),
                Some("agent_completed") => Signal::Sound(State::Done),
                // auth_success, elicitation_complete, and anything added later.
                _ => Signal::Ignore,
            },

            "StopFailure" => match classify_stop_failure(obj) {
                ErrorClass::RateLimited => Signal::Sound(State::RateLimited),
                ErrorClass::Other => Signal::Sound(State::Failed),
            },

            // `is_interrupt` marks a tool the *user* stopped. That is not a
            // failure — they did it deliberately, and they are plainly at the
            // keyboard — so it must never make a noise. Confirmed against a
            // live session; the payload also carries `error` and `duration_ms`.
            "PostToolUseFailure" => {
                if obj
                    .get("is_interrupt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    Signal::Ignore
                } else {
                    Signal::Sound(State::ToolFailed)
                }
            }
            "SubagentStop" => Signal::Sound(State::SubagentDone),
            "PreCompact" => Signal::Sound(State::Compacting),
            "SessionStart" => Signal::Sound(State::SessionStart),

            "UserPromptSubmit" => Signal::TurnStart,
            "SessionEnd" => Signal::SessionEnd,

            _ => Signal::Ignore,
        };

        let project = string_at(obj, "cwd")
            .map(PathBuf::from)
            .unwrap_or_else(cwd_or_root);

        Some(Event {
            signal,
            // Claude Code always sends a session id. Should one ever arrive
            // without, derive it from the project rather than using a shared
            // literal — otherwise two agents share one state file and each
            // rate-limits the other's alerts away.
            session_id: match string_at(obj, "session_id") {
                Some(id) if !id.is_empty() => id.to_string(),
                _ => format!(
                    "unknown-{:x}",
                    crate::core::identity::fnv1a(project.as_os_str().as_encoded_bytes())
                ),
            },
            project,
            agent: "claude-code",
        })
    }
}

enum ErrorClass {
    /// Wait it out or go pay someone. Worth its own sound.
    RateLimited,
    Other,
}

/// Keys the `StopFailure` discriminator might live under.
///
/// The event's matcher is documented to filter on the error type, but the
/// payload schema itself is not published. Rather than guess one name, consult
/// every plausible one and fall back to a plain failure — which is the correct
/// behaviour even once the real name is confirmed.
const ERROR_TYPE_KEYS: [&str; 4] = ["error_type", "stop_failure_type", "reason", "type"];

/// The subset a user can respond to by waiting or by fixing billing/auth.
const RATE_LIMITED: [&str; 4] = [
    "rate_limit",
    "overloaded",
    "billing_error",
    "authentication_failed",
];

fn classify_stop_failure(obj: &Map<String, Value>) -> ErrorClass {
    let found = ERROR_TYPE_KEYS
        .iter()
        .find_map(|key| string_at(obj, key))
        .or_else(|| {
            obj.get("error")?
                .as_object()
                .and_then(|e| string_at(e, "type"))
        });

    match found {
        Some(kind) if RATE_LIMITED.contains(&kind) => ErrorClass::RateLimited,
        _ => ErrorClass::Other,
    }
}

fn string_at<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)?.as_str()
}

fn cwd_or_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}
