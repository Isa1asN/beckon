//! The normalized vocabulary every adapter maps onto.
//!
//! Agent hook surfaces are wildly uneven — Claude Code exposes ~30 lifecycle
//! events, Codex exposes a single "turn complete" callback. Flattening them
//! onto these nine states is what makes a sound pack portable.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What the agent is telling you, independent of which agent it is.
///
/// Serializes as kebab-case. That string is the wire format everywhere: config
/// keys, pack manifest keys, and trace output all use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Turn finished; the ball is in your court.
    Done,
    /// Blocked awaiting a human decision.
    NeedsYou,
    /// The turn ended badly.
    Failed,
    /// API throttle, overload, billing or auth failure.
    RateLimited,
    /// The agent has been waiting on you a while.
    IdleWaiting,
    /// A subagent finished.
    SubagentDone,
    /// Context compaction starting.
    Compacting,
    /// Session opened.
    SessionStart,
    /// An individual tool call failed.
    ToolFailed,
}

impl State {
    pub const ALL: [State; 9] = [
        State::Done,
        State::NeedsYou,
        State::Failed,
        State::RateLimited,
        State::IdleWaiting,
        State::SubagentDone,
        State::Compacting,
        State::SessionStart,
        State::ToolFailed,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            State::Done => "done",
            State::NeedsYou => "needs-you",
            State::Failed => "failed",
            State::RateLimited => "rate-limited",
            State::IdleWaiting => "idle-waiting",
            State::SubagentDone => "subagent-done",
            State::Compacting => "compacting",
            State::SessionStart => "session-start",
            State::ToolFailed => "tool-failed",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// The next state to try when a pack does not define this one.
    ///
    /// `None` means silence is the terminus. Note that `SubagentDone`
    /// deliberately does *not* fall back to `Done`: hearing the "come look,
    /// it's finished" chime for something that is not finished trains people
    /// to ignore it.
    pub fn fallback(&self) -> Option<State> {
        match self {
            State::RateLimited => Some(State::Failed),
            State::IdleWaiting => Some(State::NeedsYou),
            _ => None,
        }
    }

    /// Core states must be defined by every pack; the rest are optional.
    pub fn is_core(&self) -> bool {
        matches!(self, State::Done | State::NeedsYou | State::Failed)
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pad`, not `write_str`: the latter silently drops width and
        // alignment, which makes every padded column in `doctor` ragged.
        f.pad(self.as_str())
    }
}

/// What an adapter extracts from one hook invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// Make this sound, subject to policy.
    Sound(State),
    /// Record the turn start timestamp. No sound.
    TurnStart,
    /// Prune this session's state. No sound.
    SessionEnd,
    /// Recognized but deliberately inert.
    Ignore,
}

/// One normalized hook invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub signal: Signal,
    pub session_id: String,
    /// The agent's working directory — used for project config and identity.
    pub project: PathBuf,
    pub agent: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_case_is_the_wire_format() {
        assert_eq!(State::NeedsYou.as_str(), "needs-you");
        assert_eq!(State::RateLimited.as_str(), "rate-limited");
        assert_eq!(State::parse("subagent-done"), Some(State::SubagentDone));
        assert_eq!(State::parse("needsYou"), None);
        assert_eq!(State::parse(""), None);
    }

    #[test]
    fn every_state_round_trips_through_its_string() {
        for s in State::ALL {
            assert_eq!(State::parse(s.as_str()), Some(s), "{s:?} failed round trip");
        }
    }

    #[test]
    fn all_contains_every_variant_exactly_once() {
        let mut seen = std::collections::BTreeSet::new();
        for s in State::ALL {
            assert!(seen.insert(s), "{s:?} appears twice in ALL");
        }
        assert_eq!(seen.len(), 9);
    }

    #[test]
    fn serde_uses_the_same_kebab_case_form() {
        assert_eq!(
            serde_json::to_string(&State::IdleWaiting).unwrap(),
            "\"idle-waiting\""
        );
        let back: State = serde_json::from_str("\"tool-failed\"").unwrap();
        assert_eq!(back, State::ToolFailed);
    }

    #[test]
    fn serde_and_as_str_never_disagree() {
        for s in State::ALL {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()), "{s:?} mismatch");
        }
    }

    #[test]
    fn fallback_chain_matches_the_spec() {
        assert_eq!(State::RateLimited.fallback(), Some(State::Failed));
        assert_eq!(State::IdleWaiting.fallback(), Some(State::NeedsYou));
        assert_eq!(State::Done.fallback(), None);
        assert_eq!(State::NeedsYou.fallback(), None);
        assert_eq!(State::Failed.fallback(), None);
    }

    #[test]
    fn subagent_done_does_not_fall_back_to_done() {
        assert_eq!(State::SubagentDone.fallback(), None);
        assert_eq!(State::Compacting.fallback(), None);
        assert_eq!(State::SessionStart.fallback(), None);
        assert_eq!(State::ToolFailed.fallback(), None);
    }

    #[test]
    fn exactly_three_states_are_core() {
        let core: Vec<_> = State::ALL.into_iter().filter(State::is_core).collect();
        assert_eq!(core, vec![State::Done, State::NeedsYou, State::Failed]);
    }

    #[test]
    fn every_fallback_chain_terminates() {
        // A cycle here would hang pack resolution.
        for s in State::ALL {
            let mut cur = s;
            for _ in 0..State::ALL.len() + 1 {
                match cur.fallback() {
                    Some(next) => cur = next,
                    None => break,
                }
            }
            assert!(cur.fallback().is_none(), "{s:?} chain did not terminate");
        }
    }

    #[test]
    fn every_fallback_terminus_is_a_core_state() {
        // Otherwise a pack could satisfy validation and still resolve to nothing.
        for s in State::ALL {
            let mut cur = s;
            while let Some(next) = cur.fallback() {
                cur = next;
            }
            if cur != s {
                assert!(cur.is_core(), "{s:?} falls back to non-core {cur:?}");
            }
        }
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(State::NeedsYou.to_string(), "needs-you");
    }

    #[test]
    fn display_honours_width_and_alignment() {
        // `doctor` and `test` lay states out in columns; a Display impl built
        // on `write_str` ignores width and silently produces ragged output.
        assert_eq!(format!("[{:<14}]", State::Done), "[done          ]");
        assert_eq!(format!("[{:>10}]", State::Done), "[      done]");
        assert_eq!(format!("[{:^8}]", State::Done), "[  done  ]");
    }
}
