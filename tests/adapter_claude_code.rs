//! Every row of the Claude Code mapping table, pinned to a fixture.
//!
//! Hook payload schemas drift and some are undocumented, so these fixtures are
//! the contract. When a real payload disagrees with one, the fixture is what
//! gets corrected — see `BECKON_DUMP`.

use beckon_cli::adapter::{adapter_for, Adapter, ClaudeCode};
use beckon_cli::core::event::{Event, Signal, State};

fn parse(fixture: &str) -> Event {
    let path = format!("tests/fixtures/claude_code/{fixture}.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    ClaudeCode
        .parse(&bytes)
        .unwrap_or_else(|| panic!("{fixture}: adapter returned None"))
}

fn payload(extra: &str) -> String {
    format!(r#"{{"session_id":"s","cwd":"/p",{extra}}}"#)
}

#[test]
fn mapping_table_is_implemented_exactly() {
    let cases = [
        ("stop", Signal::Sound(State::Done)),
        ("notification_permission", Signal::Sound(State::NeedsYou)),
        ("notification_needs_input", Signal::Sound(State::NeedsYou)),
        ("notification_elicitation", Signal::Sound(State::NeedsYou)),
        ("notification_idle", Signal::Sound(State::IdleWaiting)),
        ("notification_completed", Signal::Sound(State::Done)),
        ("stopfailure_rate_limit", Signal::Sound(State::RateLimited)),
        ("stopfailure_server_error", Signal::Sound(State::Failed)),
        ("post_tool_use_failure", Signal::Sound(State::ToolFailed)),
        ("subagent_stop", Signal::Sound(State::SubagentDone)),
        ("pre_compact", Signal::Sound(State::Compacting)),
        ("session_start", Signal::Sound(State::SessionStart)),
        ("user_prompt_submit", Signal::TurnStart),
        ("session_end", Signal::SessionEnd),
        ("notification_auth_success", Signal::Ignore),
        ("post_tool_use_failure_interrupt", Signal::Ignore),
    ];
    for (fixture, expected) in cases {
        assert_eq!(
            parse(fixture).signal,
            expected,
            "fixture {fixture} mapped wrong"
        );
    }
}

#[test]
fn every_documented_stopfailure_error_type_is_classified() {
    // rate-limited is for the failures you act on by waiting or paying; the
    // rest are plain failures.
    let rate_limited = [
        "rate_limit",
        "overloaded",
        "billing_error",
        "authentication_failed",
    ];
    let plain = [
        "invalid_request",
        "model_not_found",
        "server_error",
        "max_output_tokens",
        "unknown",
        "oauth_org_not_allowed",
    ];
    for e in rate_limited {
        let p = payload(&format!(
            r#""hook_event_name":"StopFailure","error_type":"{e}""#
        ));
        assert_eq!(
            ClaudeCode.parse(p.as_bytes()).unwrap().signal,
            Signal::Sound(State::RateLimited),
            "{e} should be rate-limited"
        );
    }
    for e in plain {
        let p = payload(&format!(
            r#""hook_event_name":"StopFailure","error_type":"{e}""#
        ));
        assert_eq!(
            ClaudeCode.parse(p.as_bytes()).unwrap().signal,
            Signal::Sound(State::Failed),
            "{e} should be a plain failure"
        );
    }
}

#[test]
fn stopfailure_degrades_to_failed_when_the_payload_shape_is_unknown() {
    // The StopFailure schema is not published. Never panic, never vanish.
    assert_eq!(
        parse("stopfailure_unknown_shape").signal,
        Signal::Sound(State::Failed)
    );
}

#[test]
fn stopfailure_discriminator_is_found_under_any_plausible_key() {
    for key in ["error_type", "stop_failure_type", "reason", "type"] {
        let p = payload(&format!(
            r#""hook_event_name":"StopFailure","{key}":"rate_limit""#
        ));
        assert_eq!(
            ClaudeCode.parse(p.as_bytes()).unwrap().signal,
            Signal::Sound(State::RateLimited),
            "key {key} was not consulted"
        );
    }
    let nested = payload(r#""hook_event_name":"StopFailure","error":{"type":"overloaded"}"#);
    assert_eq!(
        ClaudeCode.parse(nested.as_bytes()).unwrap().signal,
        Signal::Sound(State::RateLimited),
        "nested error.type was not consulted"
    );
}

#[test]
fn a_user_interrupted_tool_is_not_a_failure() {
    // Ctrl-C is a deliberate act by someone who is obviously at the keyboard.
    // Sounding an error at them would be actively insulting.
    assert_eq!(
        parse("post_tool_use_failure_interrupt").signal,
        Signal::Ignore
    );
    assert_eq!(
        parse("post_tool_use_failure").signal,
        Signal::Sound(State::ToolFailed)
    );
}

#[test]
fn a_tool_failure_without_the_interrupt_flag_still_sounds() {
    // Older payloads, or agents that do not send the field at all.
    let p = payload(r#""hook_event_name":"PostToolUseFailure","tool_name":"Bash""#);
    assert_eq!(
        ClaudeCode.parse(p.as_bytes()).unwrap().signal,
        Signal::Sound(State::ToolFailed)
    );
}

#[test]
fn an_unknown_notification_type_is_ignored_rather_than_guessed() {
    let p = payload(r#""hook_event_name":"Notification","notification_type":"brand_new_thing""#);
    assert_eq!(
        ClaudeCode.parse(p.as_bytes()).unwrap().signal,
        Signal::Ignore
    );
}

#[test]
fn a_notification_with_no_type_is_ignored() {
    let p = payload(r#""hook_event_name":"Notification""#);
    assert_eq!(
        ClaudeCode.parse(p.as_bytes()).unwrap().signal,
        Signal::Ignore
    );
}

#[test]
fn common_fields_are_extracted() {
    let e = parse("stop");
    assert_eq!(e.session_id, "s1");
    assert_eq!(e.project, std::path::Path::new("/home/dev/proj"));
    assert_eq!(e.agent, "claude-code");
}

#[test]
fn a_missing_session_id_is_derived_from_the_project() {
    // Claude Code always sends one. If a payload ever arrives without, two
    // agents must not share a state file — one's alert would rate-limit the
    // other's away, which is exactly what per-session state exists to prevent.
    let a = ClaudeCode
        .parse(br#"{"cwd":"/home/dev/api","hook_event_name":"Stop"}"#)
        .unwrap()
        .session_id;
    let b = ClaudeCode
        .parse(br#"{"cwd":"/home/dev/web","hook_event_name":"Stop"}"#)
        .unwrap()
        .session_id;

    assert!(a.starts_with("unknown-"), "{a}");
    assert_ne!(a, b, "two projects shared a session id");

    // Stable: the same project always lands on the same id.
    let again = ClaudeCode
        .parse(br#"{"cwd":"/home/dev/api","hook_event_name":"Stop"}"#)
        .unwrap()
        .session_id;
    assert_eq!(a, again);
}

#[test]
fn an_empty_session_id_is_treated_as_missing() {
    let id = ClaudeCode
        .parse(br#"{"session_id":"","cwd":"/p","hook_event_name":"Stop"}"#)
        .unwrap()
        .session_id;
    assert!(id.starts_with("unknown-"), "{id}");
}

#[test]
fn missing_cwd_falls_back_to_the_process_working_directory() {
    let p = r#"{"session_id":"s","hook_event_name":"Stop"}"#;
    let e = ClaudeCode.parse(p.as_bytes()).unwrap();
    assert!(e.project.is_absolute(), "{:?} is not absolute", e.project);
}

#[test]
fn unrecognized_events_are_ignored_not_dropped() {
    // Ignore, not None: the event was understood, it just has no sound.
    let p = payload(r#""hook_event_name":"FileChanged""#);
    assert_eq!(
        ClaudeCode.parse(p.as_bytes()).unwrap().signal,
        Signal::Ignore
    );
}

#[test]
fn malformed_input_returns_none_rather_than_panicking() {
    for bad in [
        &b""[..],
        &b"not json"[..],
        &b"[]"[..],
        &b"null"[..],
        &b"{}"[..],
        &b"{\"hook_event_name\":42}"[..],
        &[0xff, 0xfe][..],
    ] {
        assert!(ClaudeCode.parse(bad).is_none(), "{bad:?} should not parse");
    }
}

#[test]
fn registry_resolves_known_agents_only() {
    assert_eq!(
        adapter_for("claude-code").map(|a| a.id()),
        Some("claude-code")
    );
    assert_eq!(adapter_for("claude").map(|a| a.id()), Some("claude-code"));
    assert!(adapter_for("nope").is_none());
    assert!(adapter_for("").is_none());
}

#[test]
fn dump_mode_appends_raw_payloads_for_adapter_development() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dump.jsonl");
    beckon_cli::adapter::dump_to(&file, br#"{"hook_event_name":"Stop"}"#);
    beckon_cli::adapter::dump_to(&file, br#"{"hook_event_name":"StopFailure"}"#);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("StopFailure"));
}

#[test]
fn dump_to_an_unwritable_path_is_silent() {
    beckon_cli::adapter::dump_to(std::path::Path::new("/proc/nope/dump.jsonl"), b"{}");
}

#[test]
fn every_fixture_parses_and_no_fixture_is_unused() {
    // Guards against adding a fixture and forgetting to assert on it.
    let mut on_disk: Vec<String> = std::fs::read_dir("tests/fixtures/claude_code")
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .map(|e| e.file_name().to_string_lossy().replace(".json", ""))
        .collect();
    on_disk.sort();
    assert_eq!(on_disk.len(), 17, "fixture count changed: {on_disk:?}");
    for name in &on_disk {
        assert!(
            ClaudeCode
                .parse(&std::fs::read(format!("tests/fixtures/claude_code/{name}.json")).unwrap())
                .is_some(),
            "{name} failed to parse"
        );
    }
}
