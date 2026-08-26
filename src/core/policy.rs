//! When beckon stays silent.
//!
//! This is the whole etiquette rule, as one pure function with no clock, no
//! I/O and no globals — so every branch is trivially testable and the ordering
//! is auditable at a glance.
//!
//! The rule that matters most: chiming on every turn end is how these tools get
//! uninstalled by Thursday. A turn that finished in four seconds means you were
//! still watching. But a permission prompt stalls *all* progress, so it is never
//! duration-gated no matter how fast it arrived.

use crate::core::config::{Config, QuietAction};
use crate::core::event::State;
use chrono::{DateTime, Local, Utc};

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Play { state: State, volume: f32 },
    Suppress(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Disabled,
    Muted,
    QuietHours,
    EventOff,
    RateLimited,
    TooShort,
}

impl Reason {
    /// Stable kebab-case name, used in traces and by `beckon doctor`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::Disabled => "disabled",
            Reason::Muted => "muted",
            Reason::QuietHours => "quiet-hours",
            Reason::EventOff => "event-off",
            Reason::RateLimited => "rate-limited",
            Reason::TooShort => "too-short",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// Everything `decide` is allowed to look at.
pub struct PolicyInput<'a> {
    pub state: State,
    pub config: &'a Config,
    /// Local time, because quiet hours are a wall-clock concept.
    pub now: DateTime<Local>,
    pub muted_until: Option<DateTime<Utc>>,
    /// When **this session** last played **this same state**.
    ///
    /// The keying is the caller's job, which keeps this function pure — but it
    /// is load-bearing. A machine-wide value silences one agent's alert because
    /// another agent happened to finish, which is the opposite of the point.
    pub last_played: Option<DateTime<Utc>>,
    /// `None` means we do not know — beckon was installed mid-session, or the
    /// session was resumed. Fails open.
    pub turn_started: Option<DateTime<Utc>>,
}

/// Ordered checks; first match wins.
///
/// The order is the contract, not an implementation detail: mute is checked
/// before the per-event switch so that "be quiet for 30 minutes" means exactly
/// that, and the rate limit is checked before the duration gate so a burst is
/// throttled even when every event in it is an alert.
pub fn decide(input: PolicyInput) -> Decision {
    let config = input.config;
    let now = input.now.with_timezone(&Utc);

    // 1. Turned off entirely.
    if !config.enabled {
        return Decision::Suppress(Reason::Disabled);
    }

    // 2. Explicitly muted, and the mute has not expired.
    if let Some(until) = input.muted_until {
        if until > now {
            return Decision::Suppress(Reason::Muted);
        }
    }

    // 3. Quiet hours either silence us or just turn us down.
    let mut volume = config.volume;
    if let Some(window) = &config.policy.quiet_hours {
        if window.contains(input.now.time()) {
            match config.policy.quiet_hours_action {
                QuietAction::Silence => return Decision::Suppress(Reason::QuietHours),
                QuietAction::Volume(quiet) => volume = quiet,
            }
        }
    }

    // 4. This particular event is switched off.
    if !config.events.enabled(input.state) {
        return Decision::Suppress(Reason::EventOff);
    }

    // 5. Collapse a repeat of the same sound from the same session. Not a
    //    machine-wide throttle: see the note on `last_played`.
    //
    // A negative elapsed means clock skew or a restored backup; treat it as
    // "long enough ago" rather than wedging beckon until the clock catches up.
    if let Some(last) = input.last_played {
        let elapsed_ms = (now - last).num_milliseconds();
        if elapsed_ms >= 0 && (elapsed_ms as u64) < config.policy.rate_limit_ms {
            return Decision::Suppress(Reason::RateLimited);
        }
    }

    // 6. The duration gate. A turn that finished quickly means you were still
    //    watching, so there is nothing to summon you to. States in
    //    `always_alert` are exempt: they stall progress regardless of timing.
    if !config.policy.always_alert.contains(&input.state) {
        if let Some(started) = input.turn_started {
            let seconds = (now - started).num_seconds();
            if seconds >= 0 && (seconds as u64) < config.policy.min_turn_seconds {
                return Decision::Suppress(Reason::TooShort);
            }
        }
        // `None` means we never saw the turn start. Fail open: an extra chime
        // beats mysterious silence.
    }

    Decision::Play {
        state: input.state,
        volume,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::QuietHours;
    use chrono::{Duration, TimeZone};

    fn at(hour: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 8, 24, hour, 0, 0).unwrap()
    }

    /// The wall-clock instant every test treats as "now".
    ///
    /// Relative timestamps must be derived from this, never from `base()`:
    /// `decide` measures elapsed time against `input.now`, so mixing the two
    /// makes a "five seconds ago" turn look hours old.
    fn base() -> DateTime<Utc> {
        at(14).with_timezone(&Utc)
    }

    /// Build a config with targeted tweaks.
    fn config(tweak: impl FnOnce(&mut Config)) -> Config {
        let mut c = Config::default();
        tweak(&mut c);
        c
    }

    /// A long turn by default, so tests opt in to the duration gate.
    fn input<'a>(state: State, config: &'a Config) -> PolicyInput<'a> {
        PolicyInput {
            state,
            config,
            now: at(14),
            muted_until: None,
            last_played: None,
            turn_started: Some(base() - Duration::seconds(300)),
        }
    }

    fn played(d: Decision) -> f32 {
        match d {
            Decision::Play { volume, .. } => volume,
            other => panic!("expected Play, got {other:?}"),
        }
    }

    #[test]
    fn a_long_turn_plays_done() {
        let c = Config::default();
        assert!(matches!(
            decide(input(State::Done, &c)),
            Decision::Play { .. }
        ));
    }

    #[test]
    fn the_played_state_is_echoed_back() {
        let c = Config::default();
        match decide(input(State::NeedsYou, &c)) {
            Decision::Play { state, .. } => assert_eq!(state, State::NeedsYou),
            other => panic!("expected Play, got {other:?}"),
        }
    }

    #[test]
    fn globally_disabled_beats_everything() {
        let c = config(|c| c.enabled = false);
        // Even a blocking alert stays silent once the user turns beckon off.
        assert_eq!(
            decide(input(State::NeedsYou, &c)),
            Decision::Suppress(Reason::Disabled)
        );
    }

    #[test]
    fn mute_suppresses_until_it_expires() {
        let c = Config::default();
        let mut i = input(State::NeedsYou, &c);
        i.muted_until = Some(base() + Duration::minutes(10));
        assert_eq!(decide(i), Decision::Suppress(Reason::Muted));

        let mut i = input(State::NeedsYou, &c);
        i.muted_until = Some(base() - Duration::minutes(10));
        assert!(
            matches!(decide(i), Decision::Play { .. }),
            "an expired mute must not linger"
        );
    }

    #[test]
    fn quiet_hours_silence_by_default() {
        let c =
            config(|c| c.policy.quiet_hours = Some("23:00-08:00".parse::<QuietHours>().unwrap()));
        let mut i = input(State::Done, &c);
        i.now = at(2);
        assert_eq!(decide(i), Decision::Suppress(Reason::QuietHours));
    }

    #[test]
    fn quiet_hours_can_lower_volume_instead_of_silencing() {
        let c = config(|c| {
            c.policy.quiet_hours = Some("23:00-08:00".parse().unwrap());
            c.policy.quiet_hours_action = QuietAction::Volume(0.2);
        });
        let mut i = input(State::Done, &c);
        i.now = at(2);
        assert!((played(decide(i)) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn quiet_hours_do_not_apply_outside_the_window() {
        let c = config(|c| c.policy.quiet_hours = Some("23:00-08:00".parse().unwrap()));
        let mut i = input(State::Done, &c);
        i.now = at(14);
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn a_disabled_event_is_silent() {
        let c = Config::default(); // tool-failed defaults off
        assert_eq!(
            decide(input(State::ToolFailed, &c)),
            Decision::Suppress(Reason::EventOff)
        );
    }

    #[test]
    fn rate_limit_applies_to_every_state_including_alerts() {
        let c = Config::default();
        for state in [
            State::Done,
            State::NeedsYou,
            State::RateLimited,
            State::Failed,
        ] {
            let mut i = input(state, &c);
            i.last_played = Some(base() - Duration::milliseconds(200));
            assert_eq!(
                decide(i),
                Decision::Suppress(Reason::RateLimited),
                "{state} should be rate limited"
            );
        }
    }

    #[test]
    fn rate_limit_releases_after_its_window() {
        let c = Config::default();
        let mut i = input(State::Done, &c);
        i.last_played = Some(base() - Duration::milliseconds(2000));
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn a_last_played_in_the_future_does_not_wedge_us_permanently() {
        // Clock skew, or a restored backup. Fail open.
        let c = Config::default();
        let mut i = input(State::Done, &c);
        i.last_played = Some(base() + Duration::hours(3));
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn short_turns_suppress_done() {
        let c = Config::default();
        let mut i = input(State::Done, &c);
        i.turn_started = Some(base() - Duration::seconds(5));
        assert_eq!(decide(i), Decision::Suppress(Reason::TooShort));
    }

    #[test]
    fn always_alert_states_bypass_the_duration_gate_but_not_the_rate_limit() {
        let c = Config::default();
        for state in [
            State::NeedsYou,
            State::RateLimited,
            State::IdleWaiting,
            State::Failed,
        ] {
            let mut i = input(state, &c);
            i.turn_started = Some(base() - Duration::seconds(1));
            assert!(
                matches!(decide(i), Decision::Play { .. }),
                "{state} must not be duration-gated"
            );
        }
    }

    #[test]
    fn subagent_done_is_duration_gated_when_enabled() {
        let c = config(|c| c.events.set(State::SubagentDone, true));
        let mut i = input(State::SubagentDone, &c);
        i.turn_started = Some(base() - Duration::seconds(2));
        assert_eq!(decide(i), Decision::Suppress(Reason::TooShort));
    }

    #[test]
    fn a_missing_turn_start_fails_open_and_plays() {
        // beckon installed mid-session, or a resumed session. Being
        // mysteriously silent is worse than one extra chime.
        let c = Config::default();
        let mut i = input(State::Done, &c);
        i.turn_started = None;
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn a_turn_start_in_the_future_does_not_underflow() {
        let c = Config::default();
        let mut i = input(State::Done, &c);
        i.turn_started = Some(base() + Duration::seconds(60));
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn play_volume_defaults_to_the_configured_volume() {
        let c = config(|c| c.volume = 0.42);
        assert!((played(decide(input(State::Done, &c))) - 0.42).abs() < 1e-6);
    }

    #[test]
    fn checks_are_evaluated_in_documented_order() {
        // Muted AND event disabled AND too short: mute wins, because it is what
        // the user most recently and deliberately asked for.
        let c = config(|c| c.events.set(State::Done, false));
        let mut i = input(State::Done, &c);
        i.muted_until = Some(base() + Duration::minutes(5));
        i.turn_started = Some(base());
        assert_eq!(decide(i), Decision::Suppress(Reason::Muted));
    }

    #[test]
    fn a_zero_min_turn_seconds_disables_the_gate() {
        let c = config(|c| c.policy.min_turn_seconds = 0);
        let mut i = input(State::Done, &c);
        i.turn_started = Some(base());
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn a_zero_rate_limit_disables_throttling() {
        let c = config(|c| c.policy.rate_limit_ms = 0);
        let mut i = input(State::Done, &c);
        i.last_played = Some(base());
        assert!(matches!(decide(i), Decision::Play { .. }));
    }

    #[test]
    fn reason_names_are_stable_and_unique() {
        let all = [
            Reason::Disabled,
            Reason::Muted,
            Reason::QuietHours,
            Reason::EventOff,
            Reason::RateLimited,
            Reason::TooShort,
        ];
        let names: std::collections::BTreeSet<_> = all.iter().map(|r| r.as_str()).collect();
        assert_eq!(names.len(), all.len());
        assert_eq!(Reason::QuietHours.to_string(), "quiet-hours");
    }
}
