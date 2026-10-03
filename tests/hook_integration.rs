//! The pipeline end to end, driven through the real binary.
//!
//! Decisions are observed through `BECKON_TRACE` rather than through audio, so
//! these tests are deterministic and silent.

use assert_cmd::Command;

/// A path as a JSON string, properly escaped.
///
/// Interpolating a Windows path straight into JSON yields `"D:\a\..."`, and
/// `\a` is not a valid JSON escape — the payload never parses, so every test
/// silently exercises the "unparseable" branch instead of what it meant to.
fn jpath(p: &std::path::Path) -> String {
    serde_json::to_string(&p.to_string_lossy()).expect("path is representable as JSON")
}

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
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY")
            .write_stdin(payload.to_string())
            .assert()
            .code(0)
            .stdout("");
    }

    /// Run a hook as if the agent were on the far end of an SSH login, and
    /// return what it printed for the agent.
    fn hook_over_ssh(&self, payload: &str) -> String {
        let out = Command::cargo_bin("beckon")
            .unwrap()
            .args(["hook", "claude-code"])
            .env("BECKON_HOME", self.home.path())
            .env("BECKON_TRACE", &self.trace)
            .env("BECKON_AUDIO", "null")
            .env("SSH_CONNECTION", "10.0.0.2 51234 10.0.0.9 22")
            .env_remove("SSH_TTY")
            .write_stdin(payload.to_string())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        String::from_utf8(out.stdout).unwrap()
    }

    fn traced(&self) -> String {
        std::fs::read_to_string(&self.trace).unwrap_or_default()
    }

    fn event(&self, body: &str) -> String {
        self.event_for("s1", body)
    }

    fn event_for(&self, session: &str, body: &str) -> String {
        format!(
            r#"{{"session_id":"{session}","cwd":{},{body}}}"#,
            jpath(self.project.path())
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

    /// A background task reporting back, as Claude Code delivers it.
    fn wakeup(&self) -> String {
        self.event(
            r#""hook_event_name":"UserPromptSubmit","prompt":"<task-notification>\n<task-id>t1</task-id>\n<status>completed</status>\n</task-notification>""#,
        )
    }

    /// Let `minutes` pass for session `s1`, by moving everything it remembers
    /// — its turn start and when it last played each sound — that far back.
    fn pass_time(&self, minutes: i64) {
        let path = self.home.path().join("state/sessions/s1.json");
        let mut state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let shift = |v: &mut serde_json::Value| {
            let when = chrono::DateTime::parse_from_rfc3339(v.as_str().unwrap()).unwrap();
            *v =
                serde_json::Value::String((when - chrono::Duration::minutes(minutes)).to_rfc3339());
        };
        if let Some(v) = state.get_mut("turn_started") {
            shift(v);
        }
        if let Some(played) = state.get_mut("last_played").and_then(|p| p.as_object_mut()) {
            played.values_mut().for_each(shift);
        }
        std::fs::write(&path, state.to_string()).unwrap();
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
fn a_background_task_reporting_back_does_not_restart_the_turn_timer() {
    // You asked ten minutes ago and walked away. A background job reports
    // back, and the agent wraps up seconds later. That is exactly when you
    // want the chime; restarting the timer on the report used to gate it as
    // a short turn.
    let e = Env::new();
    e.hook(&e.prompt());
    e.pass_time(10);
    e.hook(&e.wakeup());
    e.hook(&e.stop());
    let t = e.traced();
    assert!(t.contains("wakeup"), "trace was: {t}");
    assert!(t.contains("play done"), "trace was: {t}");
}

#[test]
fn a_second_done_right_after_the_first_stays_quiet_until_you_return() {
    // Jobs reporting back one after another each end a turn. The first chime
    // already told you; the next within the gate window is noise. Rate limit
    // off, so it is the gate being observed, not the 1.5s dedupe.
    let e = Env::new();
    e.project_config("[policy]\nrate_limit_ms = 0\n");
    e.hook(&e.prompt());
    e.pass_time(10);
    e.hook(&e.stop());
    e.hook(&e.wakeup());
    e.hook(&e.stop());
    let t = e.traced();
    assert_eq!(t.matches("play done").count(), 1, "trace was: {t}");
    assert!(t.contains("suppress too-short"), "trace was: {t}");

    // You come back later and ask again: your prompt is the new anchor, and a
    // long turn after it plays.
    e.pass_time(20);
    e.hook(&e.prompt());
    e.pass_time(5);
    e.hook(&e.stop());
    assert_eq!(e.traced().matches("play done").count(), 2);
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
        r#"{{"session_id":"s2","cwd":{},"hook_event_name":"UserPromptSubmit"}}"#,
        jpath(e.project.path())
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
            r#"{{"session_id":"s1","cwd":{},"hook_event_name":"Stop"}}"#,
            jpath(&deep)
        ))
        .assert()
        .code(0);

    assert!(
        e.traced().contains("suppress disabled"),
        "repo-root config was ignored from a subdirectory: {}",
        e.traced()
    );
}

// ------------------------------------------------------------------- remote

fn terminal_sequence(stdout: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("hook stdout is not one JSON document ({e}): {stdout:?}"));
    value["terminalSequence"]
        .as_str()
        .unwrap_or_else(|| panic!("no terminalSequence in {stdout}"))
        .to_string()
}

#[test]
fn over_ssh_the_alert_goes_to_your_terminal_instead_of_the_remote_speakers() {
    let e = Env::new();
    let out = e.hook_over_ssh(&e.permission_prompt("s1"));
    let seq = terminal_sequence(&out);
    let project = e
        .project
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(
        seq,
        format!("\x07\x1b]9;Needs you — go unblock it · {project}\x07"),
        "the default sequences are a bell and an OSC 9 notification"
    );
    let t = e.traced();
    assert!(t.contains("play needs-you"), "{t}");
    assert!(t.contains("terminal needs-you"), "{t}");
    assert!(t.contains("local audio skipped: remote"), "{t}");
}

#[test]
fn over_ssh_a_suppressed_sound_prints_nothing() {
    // The policy still decides; the route only changes where an alert goes.
    let e = Env::new();
    e.hook(&e.prompt());
    assert_eq!(e.hook_over_ssh(&e.stop()), "");
    assert!(e.traced().contains("suppress too-short"));
}

#[test]
fn remote_off_keeps_sound_local_even_over_ssh() {
    let e = Env::new();
    e.project_config("[remote]\nmode = \"off\"\n");
    assert_eq!(e.hook_over_ssh(&e.stop()), "");
    let t = e.traced();
    assert!(t.contains("play done"), "{t}");
    assert!(!t.contains("local audio skipped"), "{t}");
}

#[test]
fn remote_both_rings_the_terminal_and_plays_locally() {
    let e = Env::new();
    e.project_config("[remote]\nmode = \"both\"\nsequences = [\"osc777\"]\n");
    let out = e.hook_over_ssh(&e.stop());
    assert!(terminal_sequence(&out).starts_with("\x1b]777;notify;beckon;Done — go look"));
    assert!(!e.traced().contains("local audio skipped"));
}

#[test]
fn nothing_is_printed_on_an_event_whose_output_reaches_the_model() {
    // SessionStart output becomes context. However the agent parses it, a
    // sound tool has no business there.
    let e = Env::new();
    e.project_config("[events]\nsession-start = true\n");
    let out = e.hook_over_ssh(&e.event(r#""hook_event_name":"SessionStart","source":"startup""#));
    assert_eq!(out, "");
    // Nothing reached you, so nothing is recorded as played either.
    let t = e.traced();
    assert!(
        t.contains("dropped session-start: this event's output reaches the model"),
        "{t}"
    );
    assert!(!t.contains("play session-start"), "{t}");
}

#[test]
fn an_alert_with_no_route_is_not_recorded_as_played() {
    // Terminal-only route, nothing to send: silent, and must stay out of the
    // history so it cannot rate-limit or gate the next alert.
    let e = Env::new();
    e.project_config("[remote]\nmode = \"always\"\nsequences = []\n");
    assert_eq!(e.hook_over_ssh(&e.stop()), "");
    let t = e.traced();
    assert!(t.contains("dropped done: remote.sequences is empty"), "{t}");
    let state =
        std::fs::read_to_string(e.home.path().join("state/sessions/s1.json")).unwrap_or_default();
    assert!(!state.contains("\"done\""), "recorded as played: {state}");
}

#[test]
fn over_ssh_garbage_still_prints_nothing() {
    let e = Env::new();
    for bad in ["", "not json", "{", r#"{"hook_event_name":"Stop""#] {
        assert_eq!(e.hook_over_ssh(bad), "", "{bad:?}");
    }
}
