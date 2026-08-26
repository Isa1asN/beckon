//! Choosing which sound to play for a state.
//!
//! Optional states fall back down a chain so a pack only has to define the
//! three core sounds. Silence is a legitimate terminus — `subagent-done`
//! resolving to nothing is correct, and better than borrowing the completion
//! chime for something that is not complete.

use crate::core::event::State;
use crate::pack::manifest::{Pack, SoundDef};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a sound comes from once everything has been consulted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source<'a> {
    /// Defined by the active pack.
    Pack(&'a SoundDef),
    /// A file the user named directly in their own config.
    File(&'a Path),
}

/// The sound to play for `state`, and which state actually supplied it.
///
/// The returned state can differ from the requested one; callers that report to
/// the user should say which sound they actually played.
pub fn resolve(pack: &Pack, state: State) -> Option<(State, &SoundDef)> {
    let mut current = state;
    // Bounded even though `State::fallback` is proven acyclic, so a future
    // edit to the chain cannot turn this into a hang.
    for _ in 0..=State::ALL.len() {
        if let Some(def) = pack.sounds.get(&current) {
            return Some((current, def));
        }
        current = current.fallback()?;
    }
    None
}

/// Resolve with the user's own file overrides taking precedence.
///
/// Overrides are consulted at *each* step of the fallback chain, not just the
/// first: someone who replaces `failed` with their own file expects
/// `rate-limited` to fall back to that file, not to the pack's version.
pub fn resolve_with_overrides<'a>(
    pack: &'a Pack,
    overrides: &'a BTreeMap<State, PathBuf>,
    state: State,
) -> Option<(State, Source<'a>)> {
    let mut current = state;
    for _ in 0..=State::ALL.len() {
        if let Some(path) = overrides.get(&current) {
            return Some((current, Source::File(path)));
        }
        if let Some(def) = pack.sounds.get(&current) {
            return Some((current, Source::Pack(def)));
        }
        current = current.fallback()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pack_with(states: &[State]) -> Pack {
        let mut toml = String::from(
            "[pack]\nid=\"t\"\nname=\"T\"\nversion=\"1\"\nauthor=\"a\"\nlicense=\"CC0-1.0\"\n",
        );
        for s in states {
            toml.push_str(&format!(
                "[sounds.\"{}\"]\ntype=\"synth\"\n[[sounds.\"{}\".layer]]\nwave=\"sine\"\nnotes=[440.0]\n",
                s.as_str(),
                s.as_str()
            ));
        }
        Pack::parse(&toml, None::<PathBuf>).expect("fixture should parse")
    }

    fn no_overrides() -> BTreeMap<State, PathBuf> {
        BTreeMap::new()
    }

    #[test]
    fn an_override_beats_the_pack() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        let mut overrides = no_overrides();
        overrides.insert(State::Done, PathBuf::from("/mine/ding.wav"));

        let (source_state, source) = resolve_with_overrides(&p, &overrides, State::Done).unwrap();
        assert_eq!(source_state, State::Done);
        assert_eq!(source, Source::File(Path::new("/mine/ding.wav")));
    }

    #[test]
    fn states_without_an_override_still_come_from_the_pack() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        let mut overrides = no_overrides();
        overrides.insert(State::Done, PathBuf::from("/mine/ding.wav"));

        let (_, source) = resolve_with_overrides(&p, &overrides, State::NeedsYou).unwrap();
        assert!(matches!(source, Source::Pack(_)));
    }

    #[test]
    fn an_override_is_honoured_partway_down_the_fallback_chain() {
        // Replace `failed`, and `rate-limited` should reach your file rather
        // than the pack's `failed`.
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        let mut overrides = no_overrides();
        overrides.insert(State::Failed, PathBuf::from("/mine/oops.wav"));

        let (source_state, source) =
            resolve_with_overrides(&p, &overrides, State::RateLimited).unwrap();
        assert_eq!(source_state, State::Failed);
        assert_eq!(source, Source::File(Path::new("/mine/oops.wav")));
    }

    #[test]
    fn an_override_can_give_a_state_the_pack_leaves_silent() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        assert!(resolve_with_overrides(&p, &no_overrides(), State::Compacting).is_none());

        let mut overrides = no_overrides();
        overrides.insert(State::Compacting, PathBuf::from("/mine/shuffle.wav"));
        assert!(resolve_with_overrides(&p, &overrides, State::Compacting).is_some());
    }

    #[test]
    fn without_overrides_both_resolvers_agree() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        for state in State::ALL {
            let plain = resolve(&p, state).map(|(s, _)| s);
            let with = resolve_with_overrides(&p, &no_overrides(), state).map(|(s, _)| s);
            assert_eq!(plain, with, "{state} disagreed");
        }
    }

    #[test]
    fn a_defined_state_resolves_to_itself() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        assert_eq!(resolve(&p, State::Done).unwrap().0, State::Done);
    }

    #[test]
    fn rate_limited_falls_back_to_failed() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        assert_eq!(resolve(&p, State::RateLimited).unwrap().0, State::Failed);
    }

    #[test]
    fn idle_waiting_falls_back_to_needs_you() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        assert_eq!(resolve(&p, State::IdleWaiting).unwrap().0, State::NeedsYou);
    }

    #[test]
    fn a_defined_optional_state_wins_over_its_fallback() {
        let p = pack_with(&[State::Failed, State::RateLimited]);
        assert_eq!(
            resolve(&p, State::RateLimited).unwrap().0,
            State::RateLimited
        );
    }

    #[test]
    fn subagent_done_resolves_to_silence_rather_than_borrowing_done() {
        let p = pack_with(&[State::Done, State::NeedsYou, State::Failed]);
        assert!(resolve(&p, State::SubagentDone).is_none());
        assert!(resolve(&p, State::Compacting).is_none());
        assert!(resolve(&p, State::SessionStart).is_none());
        assert!(resolve(&p, State::ToolFailed).is_none());
    }

    #[test]
    fn an_empty_pack_resolves_nothing() {
        let p = pack_with(&[]);
        for s in State::ALL {
            assert!(resolve(&p, s).is_none(), "{s} should not resolve");
        }
    }

    #[test]
    fn a_missing_core_state_resolves_to_nothing_not_to_a_sibling() {
        let p = pack_with(&[State::Done]);
        assert!(resolve(&p, State::NeedsYou).is_none());
        assert!(resolve(&p, State::Failed).is_none());
    }
}
