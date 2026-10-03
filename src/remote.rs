//! Alerts for an agent running on another machine.
//!
//! Over SSH, a sound played on the remote host reaches nobody. What does reach
//! you is the terminal, so beckon hands the agent an escape sequence — a bell,
//! a desktop notification — through the hook's `terminalSequence` output, and
//! the agent writes it to your terminal along with everything else it draws.
//!
//! Through the agent rather than straight to `/dev/tty`: the agent owns that
//! terminal and is redrawing it, and bytes written underneath it can land in
//! the middle of one of its own sequences.
//!
//! Best-effort by nature. Whether OSC 9 or OSC 777 becomes a notification is up
//! to your terminal; the bell is the one thing nearly every terminal honours.

use crate::core::config::{RemoteMode, Sequence};
use crate::core::event::State;

/// Where one alert goes. Every mode goes somewhere: there is no variant for
/// nowhere, so no caller has to handle one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The speakers of the machine beckon runs on.
    Local,
    /// Escape sequences for the terminal the agent is drawn in.
    Terminal,
    Both,
}

impl Route {
    pub fn local_audio(self) -> bool {
        matches!(self, Route::Local | Route::Both)
    }

    pub fn terminal(self) -> bool {
        matches!(self, Route::Terminal | Route::Both)
    }
}

/// Decide the route from the configured mode and whether we are under SSH.
pub fn route(mode: RemoteMode, ssh: bool) -> Route {
    match mode {
        RemoteMode::Off => Route::Local,
        RemoteMode::Auto if ssh => Route::Terminal,
        RemoteMode::Auto => Route::Local,
        RemoteMode::Always => Route::Terminal,
        RemoteMode::Both => Route::Both,
    }
}

/// Is this process running inside an SSH session?
///
/// `sshd` sets both for an interactive login; either is enough, since tmux and
/// some launchers pass along one and not the other.
pub fn ssh_detected() -> bool {
    ssh_in(|key| std::env::var_os(key))
}

fn ssh_in(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|key| get(key).is_some_and(|v| !v.is_empty()))
}

/// The escape sequences for one alert, in configured order.
///
/// Every body opens with the state's label, never the project name: Claude
/// Code rejects an OSC 9 whose body begins with a digit (it would read as a
/// progress report), and rejecting it drops the *whole* sequence, bell and all.
/// A project called `2048` must not cost you the alert.
pub fn sequence(sequences: &[Sequence], state: State, project: &str) -> String {
    let body = format!("{} · {}", label(state), clean(project));
    sequences
        .iter()
        .map(|s| match s {
            Sequence::Bel => "\x07".to_string(),
            Sequence::Osc9 => format!("\x1b]9;{body}\x07"),
            Sequence::Osc777 => format!("\x1b]777;notify;beckon;{body}\x07"),
        })
        .collect()
}

/// The hook's stdout for one alert: the JSON the agent reads `terminalSequence`
/// from. `None` when there is nothing to send.
pub fn hook_output(sequences: &[Sequence], state: State, project: &str) -> Option<String> {
    if sequences.is_empty() {
        return None;
    }
    let payload = serde_json::json!({ "terminalSequence": sequence(sequences, state, project) });
    Some(payload.to_string())
}

/// How a notification names the project: its root directory's name.
pub fn project_label(root: &std::path::Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The configured sequences as written in config, for `doctor` and `test`.
pub fn describe(sequences: &[Sequence]) -> String {
    sequences
        .iter()
        .map(Sequence::as_str)
        .collect::<Vec<_>>()
        .join(" + ")
}

/// What a notification says, in the README's words: what happened, and what to
/// do about it.
fn label(state: State) -> &'static str {
    match state {
        State::Done => "Done — go look",
        State::NeedsYou => "Needs you — go unblock it",
        State::Failed => "Failed — go read the error",
        State::RateLimited => "Rate-limited — wait",
        State::IdleWaiting => "Waiting on you",
        State::SubagentDone => "Subagent done",
        State::Compacting => "Compacting",
        State::SessionStart => "Session started",
        State::ToolFailed => "Tool failed",
    }
}

/// Make a project name safe inside an OSC payload.
///
/// Control characters would end or corrupt the sequence, invisible bidi and
/// format characters would make the notification read as something it is not,
/// and `;` is OSC 777's field separator — `a;b` as a title would split the notification in two.
/// Bounded, because a notification is a glance, not a log.
fn clean(name: &str) -> String {
    const MAX_CHARS: usize = 60;
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && !crate::text::hidden(*c))
        .map(|c| if c == ';' { ',' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.chars().count() <= MAX_CHARS {
        cleaned.to_string()
    } else {
        let cut: String = cleaned.chars().take(MAX_CHARS - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A port of the allowlist Claude Code (2.1.288) applies before writing a
    /// hook's `terminalSequence`: BEL, or OSC 0/1/2/9/99/777 terminated by BEL
    /// or ST, at most 4096 bytes, and no OSC 9 body that opens with a digit
    /// unless it is the `9;4` progress form. Anything else and the entire
    /// sequence is dropped, so every sequence beckon emits must pass this.
    fn agent_accepts(seq: &str) -> bool {
        if seq.is_empty() || seq.len() > 4096 {
            return false;
        }
        let chars: Vec<char> = seq.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\x07' {
                i += 1;
                continue;
            }
            if chars[i] != '\x1b' || chars.get(i + 1) != Some(&']') {
                return false;
            }
            let start = i + 2;
            let mut j = start;
            let (end, width) = loop {
                match chars.get(j) {
                    None => return false,
                    Some('\x07') => break (j, 1),
                    Some('\x1b') if chars.get(j + 1) == Some(&'\\') => break (j, 2),
                    Some('\x1b') => return false,
                    _ => j += 1,
                }
            };
            let inner: String = chars[start..end].iter().collect();
            let (ps, body) = inner.split_once(';').unwrap_or((&inner, ""));
            if ps.is_empty() || !ps.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            if ![0, 1, 2, 9, 99, 777].contains(&ps.parse::<u32>().unwrap_or(u32::MAX)) {
                return false;
            }
            if ps == "9" && !osc9_body_allowed(body) {
                return false;
            }
            i = end + width;
        }
        true
    }

    /// The OSC 9 rule exactly: control characters are stripped first, the
    /// `4;state[;percent]` progress form is allowed, and otherwise the body may
    /// not open — after whitespace, U+180E or U+200B, and one sign — with a
    /// digit of any script. Stricter than needed is fine here; looser is not.
    fn osc9_body_allowed(body: &str) -> bool {
        let body: String = body
            .chars()
            .filter(|c| !((*c as u32) < 32 || *c == '\x7f' || ('\u{80}'..='\u{9f}').contains(c)))
            .collect();
        if is_progress(&body) {
            return true;
        }
        let rest = body
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{180e}' || c == '\u{200b}');
        let rest = rest.strip_prefix(['+', '-']).unwrap_or(rest);
        !rest.starts_with(|c: char| c.is_numeric())
    }

    /// `^4;[0-4](;(100|\d{1,2})?)?$`
    fn is_progress(body: &str) -> bool {
        let Some(rest) = body.strip_prefix("4;") else {
            return false;
        };
        let mut parts = rest.splitn(2, ';');
        let state_ok = matches!(parts.next(), Some("0" | "1" | "2" | "3" | "4"));
        let percent_ok = match parts.next() {
            None | Some("") => true,
            Some("100") => true,
            Some(p) => (1..=2).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()),
        };
        state_ok && percent_ok
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    const ALL: [Sequence; 3] = [Sequence::Bel, Sequence::Osc9, Sequence::Osc777];

    #[test]
    fn the_default_keeps_sound_local_until_ssh_appears() {
        assert_eq!(route(RemoteMode::Auto, false), Route::Local);
        assert_eq!(route(RemoteMode::Auto, true), Route::Terminal);
    }

    #[test]
    fn explicit_modes_ignore_detection() {
        for ssh in [false, true] {
            assert_eq!(route(RemoteMode::Off, ssh), Route::Local);
            assert_eq!(route(RemoteMode::Always, ssh), Route::Terminal);
            assert_eq!(route(RemoteMode::Both, ssh), Route::Both);
        }
    }

    #[test]
    fn every_route_reaches_somewhere() {
        for route in [Route::Local, Route::Terminal, Route::Both] {
            assert!(route.local_audio() || route.terminal(), "{route:?}");
        }
        assert!(Route::Both.local_audio() && Route::Both.terminal());
    }

    #[test]
    fn either_ssh_variable_is_enough_and_an_empty_one_is_not() {
        assert!(ssh_in(env(&[(
            "SSH_CONNECTION",
            "10.0.0.2 51234 10.0.0.9 22"
        )])));
        assert!(ssh_in(env(&[("SSH_TTY", "/dev/pts/3")])));
        assert!(!ssh_in(env(&[])));
        assert!(!ssh_in(env(&[("SSH_CONNECTION", ""), ("SSH_TTY", "")])));
    }

    #[test]
    fn every_state_and_sequence_passes_the_agents_allowlist() {
        for state in State::ALL {
            for one in ALL {
                let seq = sequence(&[one], state, "api-server");
                assert!(agent_accepts(&seq), "{one:?} for {state} rejected: {seq:?}");
            }
            assert!(agent_accepts(&sequence(&ALL, state, "api-server")));
        }
    }

    #[test]
    fn a_hostile_or_numeric_project_name_cannot_get_the_alert_dropped() {
        for project in [
            "2048",
            "-1 things",
            "a;b;c",
            "evil\x07\x1b]0;pwned\x07",
            "\u{9b}31m",
            "rlo\u{202e}gnp.exe",
            "\u{200b}1",
            "",
        ] {
            let seq = sequence(&ALL, State::Done, project);
            assert!(agent_accepts(&seq), "{project:?} produced {seq:?}");
            assert_eq!(
                seq.matches('\x1b').count(),
                2,
                "{project:?} smuggled a sequence: {seq:?}"
            );
        }
    }

    #[test]
    fn the_allowlist_port_rejects_what_the_agent_rejects() {
        // Guards the guard: a permissive port would let a broken sequence pass.
        assert!(!agent_accepts(""));
        assert!(!agent_accepts("\x1b]9;2048 done\x07"));
        assert!(!agent_accepts("\x1b]8;;https://x\x07"));
        assert!(!agent_accepts("\x1b[31m"));
        assert!(!agent_accepts("\x1b]9;unterminated"));
        assert!(agent_accepts("\x1b]9;4;1;50\x07"));
        assert!(!agent_accepts("\x1b]9;4;garbage\x07"));
        assert!(!agent_accepts("\x1b]9;4;9;999\x07"));
        assert!(!agent_accepts("\x1b]9;\u{663} arabic-indic three\x07"));
        assert!(!agent_accepts("\x1b]9;\u{200b}1 hidden digit\x07"));
        assert!(!agent_accepts("\x1b]9;\x015 control then digit\x07"));
        assert!(agent_accepts("\x1b]777;notify;t;b\x1b\\"));
    }

    #[test]
    fn the_notification_says_what_happened_and_where() {
        let seq = sequence(&[Sequence::Osc9], State::NeedsYou, "api-server");
        assert_eq!(seq, "\x1b]9;Needs you — go unblock it · api-server\x07");
        let seq = sequence(&[Sequence::Osc777], State::Done, "web");
        assert_eq!(seq, "\x1b]777;notify;beckon;Done — go look · web\x07");
    }

    #[test]
    fn sequences_follow_the_configured_order() {
        let seq = sequence(&[Sequence::Osc9, Sequence::Bel], State::Failed, "p");
        assert!(
            seq.starts_with("\x1b]9;") && seq.ends_with("\x07\x07"),
            "{seq:?}"
        );
    }

    #[test]
    fn hook_output_is_the_json_the_agent_reads() {
        let out = hook_output(&[Sequence::Bel], State::Done, "p").unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["terminalSequence"], "\x07");
        assert_eq!(
            value.as_object().unwrap().len(),
            1,
            "nothing that could steer the agent"
        );
        assert!(hook_output(&[], State::Done, "p").is_none());
    }

    #[test]
    fn invisible_formatting_never_reaches_a_notification() {
        let seq = sequence(&[Sequence::Osc9], State::Done, "rlo\u{202e}gnp.exe\u{2066}");
        assert_eq!(seq, "\x1b]9;Done — go look · rlognp.exe\x07");
    }

    #[test]
    fn a_long_project_name_is_shortened() {
        let name = "x".repeat(500);
        let seq = sequence(&[Sequence::Osc9], State::Done, &name);
        assert!(seq.chars().count() < 100, "{}", seq.len());
        assert!(seq.contains('…'));
    }
}
