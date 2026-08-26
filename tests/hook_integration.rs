//! The pipeline end to end, driven through the real binary.
//!
//! Decisions are observed through `BECKON_TRACE` rather than through audio, so
//! these tests are deterministic and silent.

use assert_cmd::Command;

struct Env {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    trace: std::path::PathBuf,
}

impl Env {
    fn new() -> Env {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let trace = home.path().join("trace.log");
        Env {
            home,
            project,
            trace,
        }
    }

    fn hook(&self, payload: &str) {
        Command::cargo_bin("beckon")
            .unwrap()
            .args(["hook", "claude-code"])
            .env("BECKON_HOME", self.home.path())
            .env("BECKON_TRACE", &self.trace)
            .env("BECKON_AUDIO", "null")
            .write_stdin(payload.to_string())
            .assert()
            .code(0)
            .stdout("");
    }

    fn traced(&self) -> String {
        std::fs::read_to_string(&self.trace).unwrap_or_default()
    }

    fn event(&self, body: &str) -> String {
        self.event_for("s1", body)
    }

    fn event_for(&self, session: &str, body: &str) -> String {
        format!(
            r#"{{"session_id":"{session}","cwd":"{}",{body}}}"#,
            self.project.path().display()
        )
    }

    fn permission_prompt(&self, session: &str) -> String {
        self.event_for(
            session,
            r#""hook_event_name":"Notification","notification_type":"permission_prompt""#,
        )
    }

    fn stop(&self) -> String {
        self.event(r#""hook_event_name":"Stop","stop_reason":"end_turn""#)
    }

    fn prompt(&self) -> String {
        self.event(r#""hook_event_name":"UserPromptSubmit""#)
    }

    fn project_config(&self, body: &str) {
        std::fs::write(self.project.path().join(".beckon.toml"), body).unwrap();
    }

    fn sessions(&self) -> usize {
        std::fs::read_dir(self.home.path().join("state/sessions"))
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    }
}

#[test]
fn a_stop_with_no_recorded_turn_start_plays_done() {
    let e = Env::new();
    e.hook(&e.stop());
    assert!(
        e.traced().contains("play done"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn user_prompt_submit_records_turn_start_and_makes_no_sound() {
    let e = Env::new();
    e.hook(&e.prompt());
    let t = e.traced();
    assert!(t.contains("turn-start"), "trace was: {t}");
    assert!(!t.contains("play "), "UserPromptSubmit must be silent: {t}");
    assert_eq!(e.sessions(), 1);
}

#[test]
fn a_short_turn_suppresses_done() {
    let e = Env::new();
    e.hook(&e.prompt());
    e.hook(&e.stop());
    assert!(
        e.traced().contains("suppress too-short"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn a_blocking_alert_is_not_duration_gated() {
    let e = Env::new();
    e.hook(&e.prompt());
    e.hook(&e.event(r#""hook_event_name":"Notification","notification_type":"permission_prompt""#));
    assert!(
        e.traced().contains("play needs-you"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn the_rate_limit_suppresses_a_rapid_second_sound() {
    let e = Env::new();
    e.hook(&e.stop());
    e.hook(&e.stop());
    let t = e.traced();
    assert_eq!(t.matches("play done").count(), 1, "trace was: {t}");
    assert!(t.contains("suppress rate-limited"), "trace was: {t}");
}

#[test]
fn session_end_prunes_that_sessions_state() {
    let e = Env::new();
    e.hook(&e.prompt());
    assert_eq!(e.sessions(), 1);
    e.hook(&e.event(r#""hook_event_name":"SessionEnd","session_end_reason":"clear""#));
    assert_eq!(e.sessions(), 0);
}

#[test]
fn a_project_config_disables_beckon_for_that_project_only() {
    let e = Env::new();
    e.project_config("enabled = false\n");
    e.hook(&e.stop());
    assert!(
        e.traced().contains("suppress disabled"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn a_default_off_event_is_silent_until_the_project_enables_it() {
    let e = Env::new();
    let payload = e.event(r#""hook_event_name":"PostToolUseFailure","tool_name":"Bash""#);
    e.hook(&payload);
    assert!(
        e.traced().contains("suppress event-off"),
        "trace was: {}",
        e.traced()
    );

    e.project_config("[events]\ntool-failed = true\n[policy]\nrate_limit_ms = 0\n");
    e.hook(&payload);
    assert!(
        e.traced().contains("play tool-failed"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn parallel_sessions_keep_independent_turn_timers() {
    let e = Env::new();
    // s2 starts a turn; s1 has no record and must fail open.
    e.hook(&format!(
        r#"{{"session_id":"s2","cwd":"{}","hook_event_name":"UserPromptSubmit"}}"#,
        e.project.path().display()
    ));
    e.hook(&e.stop());
    assert!(
        e.traced().contains("play done"),
        "trace was: {}",
        e.traced()
    );
    // Two files now: s2 has a turn start, s1 has a played record. The point
    // stands — s1's missing turn start did not borrow s2's.
    assert_eq!(e.sessions(), 2);
}

#[test]
fn an_ignored_event_is_traced_as_ignored_not_played() {
    let e = Env::new();
    e.hook(&e.event(r#""hook_event_name":"FileChanged""#));
    let t = e.traced();
    assert!(t.contains("ignore"), "trace was: {t}");
    assert!(!t.contains("play "), "trace was: {t}");
}

#[test]
fn an_unparseable_payload_is_distinguishable_from_an_ignored_one() {
    let e = Env::new();
    e.hook("not json");
    assert!(
        e.traced().contains("unparseable"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn an_unknown_agent_is_traced_and_harmless() {
    let e = Env::new();
    Command::cargo_bin("beckon")
        .unwrap()
        .args(["hook", "no-such-agent"])
        .env("BECKON_HOME", e.home.path())
        .env("BECKON_TRACE", &e.trace)
        .write_stdin("{}")
        .assert()
        .code(0)
        .stdout("");
    assert!(
        e.traced().contains("unknown-agent"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn a_malformed_project_config_does_not_stop_the_sound() {
    // Failing open: a typo must not silence the tool.
    let e = Env::new();
    e.project_config("this is not valid toml {{{");
    e.hook(&e.stop());
    assert!(
        e.traced().contains("play done"),
        "trace was: {}",
        e.traced()
    );
}

#[test]
fn tracing_is_off_unless_beckon_trace_is_set() {
    let e = Env::new();
    Command::cargo_bin("beckon")
        .unwrap()
        .args(["hook", "claude-code"])
        .env("BECKON_HOME", e.home.path())
        .env_remove("BECKON_TRACE")
        .write_stdin(e.stop())
        .assert()
        .code(0)
        .stdout("");
    assert!(!e.trace.exists(), "trace file should not have been created");
}

// ── the rate limit must not cross session boundaries ──────────────────────
//
// People run several agents in parallel worktrees; that is the workflow this
// tool exists for. Their turn boundaries are correlated, not independent, so a
// machine-wide throttle collapses exactly the bursts that carry the most
// information.

#[test]
fn one_agents_chime_does_not_silence_anothers_alert() {
    let e = Env::new();
    e.hook(&e.event_for("agent-A", r#""hook_event_name":"Stop""#));
    e.hook(&e.permission_prompt("agent-B"));
    let t = e.traced();
    assert!(t.contains("play done"), "trace was: {t}");
    assert!(
        t.contains("play needs-you"),
        "agent B's alert was swallowed by agent A's chime: {t}"
    );
}

#[test]
fn four_parallel_agents_each_get_their_own_alert() {
    let e = Env::new();
    for session in ["w1", "w2", "w3", "w4"] {
        e.hook(&e.permission_prompt(session));
    }
    let t = e.traced();
    assert_eq!(
        t.matches("play needs-you").count(),
        4,
        "four blocked agents must produce four alerts: {t}"
    );
}

#[test]
fn a_repeated_state_in_one_session_is_still_deduped() {
    // This is the burst the limit legitimately earns its keep on: Stop and
    // Notification/agent_completed both map to `done`.
    let e = Env::new();
    e.hook(&e.event_for("s1", r#""hook_event_name":"Stop""#));
    e.hook(&e.event_for(
        "s1",
        r#""hook_event_name":"Notification","notification_type":"agent_completed""#,
    ));
    let t = e.traced();
    assert_eq!(t.matches("play done").count(), 1, "trace was: {t}");
    assert!(t.contains("suppress rate-limited"), "trace was: {t}");
}

#[test]
fn a_different_state_in_the_same_session_is_not_deduped() {
    // A different state always carries new information.
    let e = Env::new();
    e.hook(&e.event_for("s1", r#""hook_event_name":"Stop""#));
    e.hook(&e.permission_prompt("s1"));
    let t = e.traced();
    assert!(t.contains("play done"), "trace was: {t}");
    assert!(t.contains("play needs-you"), "trace was: {t}");
}

#[test]
fn a_project_config_applies_from_a_subdirectory() {
    // Agents are routinely launched from somewhere below the repo root.
    let e = Env::new();
    e.project_config("enabled = false\n");
    let deep = e.project.path().join("crates/inner/src");
    std::fs::create_dir_all(&deep).unwrap();

    Command::cargo_bin("beckon")
        .unwrap()
        .args(["hook", "claude-code"])
        .env("BECKON_HOME", e.home.path())
        .env("BECKON_TRACE", &e.trace)
        .write_stdin(format!(
            r#"{{"session_id":"s1","cwd":"{}","hook_event_name":"Stop"}}"#,
            deep.display()
        ))
        .assert()
        .code(0);

    assert!(
        e.traced().contains("suppress disabled"),
        "repo-root config was ignored from a subdirectory: {}",
        e.traced()
    );
}
