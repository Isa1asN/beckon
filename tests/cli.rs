//! The human-facing subcommands.
//!
//! Every case forces `BECKON_AUDIO=null`, so the suite is silent on a
//! developer's machine and in CI.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn beckon(home: &tempfile::TempDir) -> Command {
    let mut c = Command::cargo_bin("beckon").unwrap();
    c.env("BECKON_HOME", home.path())
        .env("BECKON_AUDIO", "null");
    c
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn help_lists_the_commands_a_person_would_use() {
    Command::cargo_bin("beckon")
        .unwrap()
        .arg("--help")
        .assert()
        .code(0)
        .stdout(contains("test"))
        .stdout(contains("doctor"));
}

#[test]
fn internal_commands_stay_out_of_help() {
    // `hook` and `__play` are for the agent, not for people.
    Command::cargo_bin("beckon")
        .unwrap()
        .arg("--help")
        .assert()
        .stdout(contains("__play").not())
        .stdout(contains("lifecycle hook payload").not());
}

#[test]
fn doctor_reports_the_active_pack_and_backend() {
    let h = home();
    beckon(&h)
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("aurora"))
        .stdout(contains("backend"))
        .stdout(contains("all nine states"));
}

#[test]
fn doctor_says_so_when_the_configured_pack_is_missing() {
    let h = home();
    std::fs::write(h.path().join("config.toml"), "pack = \"ghost\"\n").unwrap();
    beckon(&h)
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("NOT FOUND"));
}

#[test]
fn doctor_surfaces_config_warnings() {
    let h = home();
    std::fs::write(h.path().join("config.toml"), "packk = \"cipher\"\n").unwrap();
    beckon(&h)
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("warnings"))
        .stdout(contains("packk"));
}

#[test]
fn test_plays_a_single_state() {
    let h = home();
    beckon(&h)
        .args(["test", "cipher", "--state", "needs-you"])
        .assert()
        .code(0)
        .stdout(contains("Cipher"))
        .stdout(contains("needs-you"));
}

#[test]
fn test_covers_every_state_by_default() {
    let h = home();
    let out = beckon(&h)
        .args(["test", "unit-7"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    for state in [
        "done",
        "needs-you",
        "failed",
        "rate-limited",
        "idle-waiting",
    ] {
        assert!(text.contains(state), "{state} missing from: {text}");
    }
}

#[test]
fn test_reports_which_project_it_is_imitating() {
    let h = home();
    beckon(&h)
        .args(["test", "aurora", "--state", "done", "--here"])
        .assert()
        .code(0)
        .stdout(contains("semitones"));
}

#[test]
fn an_unknown_pack_fails_with_a_pointer_to_what_exists() {
    let h = home();
    beckon(&h)
        .args(["test", "no-such-pack"])
        .assert()
        .code(1)
        .stderr(contains("no pack named"))
        .stderr(contains("beckon packs"));
}

#[test]
fn an_unknown_state_lists_the_valid_ones() {
    let h = home();
    beckon(&h)
        .args(["test", "--state", "needsyou"])
        .assert()
        .code(2)
        .stderr(contains("needs-you"))
        .stderr(contains("rate-limited"));
}

#[test]
fn an_installed_pack_shadows_a_builtin_end_to_end() {
    let h = home();
    let dir = h.path().join("data/packs/aurora");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pack.toml"),
        "[pack]\nid=\"aurora\"\nname=\"My Aurora\"\nversion=\"1\"\nauthor=\"me\"\n\
         license=\"CC0-1.0\"\n[sounds.done]\ntype=\"synth\"\n\
         [[sounds.done.layer]]\nwave=\"sine\"\nnotes=[440.0]\n",
    )
    .unwrap();
    beckon(&h)
        .args(["test", "aurora"])
        .assert()
        .code(0)
        .stdout(contains("My Aurora"));
}

// ── packs / use ───────────────────────────────────────────────────────────

#[test]
fn packs_lists_every_builtin_and_marks_the_active_one() {
    let h = home();
    let out = beckon(&h)
        .arg("packs")
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    for id in ["aurora", "cipher", "unit-7"] {
        assert!(text.contains(id), "{id} missing from:\n{text}");
    }
    let active: Vec<&str> = text.lines().filter(|l| l.starts_with('*')).collect();
    assert_eq!(
        active.len(),
        1,
        "exactly one pack should be marked active:\n{text}"
    );
    assert!(active[0].contains("aurora"));
}

#[test]
fn use_switches_the_pack_and_it_persists() {
    let h = home();
    beckon(&h)
        .args(["use", "cipher"])
        .assert()
        .code(0)
        .stdout(contains("Cipher"));

    // A separate process must see the change — this is config on disk, not state.
    beckon(&h)
        .arg("config")
        .args(["get", "pack"])
        .assert()
        .code(0)
        .stdout(contains("cipher"));
    let out = beckon(&h)
        .arg("packs")
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.lines()
            .any(|l| l.starts_with('*') && l.contains("cipher")),
        "{text}"
    );
}

#[test]
fn use_refuses_an_unknown_pack_and_changes_nothing() {
    let h = home();
    beckon(&h)
        .args(["use", "nonexistent"])
        .assert()
        .code(1)
        .stderr(contains("no pack named"))
        .stderr(contains("aurora"));
    beckon(&h)
        .arg("config")
        .args(["get", "pack"])
        .assert()
        .stdout(contains("aurora"));
}

// ── mute ──────────────────────────────────────────────────────────────────

#[test]
fn mute_defaults_to_half_an_hour() {
    let h = home();
    beckon(&h)
        .arg("mute")
        .assert()
        .code(0)
        .stdout(contains("30m"));
}

#[test]
fn mute_accepts_a_duration_and_unmute_ends_it() {
    let h = home();
    beckon(&h)
        .args(["mute", "2h"])
        .assert()
        .code(0)
        .stdout(contains("2h"));
    beckon(&h)
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("muted     until"));

    beckon(&h)
        .arg("unmute")
        .assert()
        .code(0)
        .stdout(contains("Unmuted"));
    beckon(&h)
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("muted     no"));
}

#[test]
fn unmute_when_not_muted_says_so_rather_than_failing() {
    let h = home();
    beckon(&h)
        .arg("unmute")
        .assert()
        .code(0)
        .stdout(contains("Not muted"));
}

#[test]
fn mute_refuses_a_duration_it_cannot_read() {
    let h = home();
    // `-5m` is excluded: clap claims it as a flag before we ever see it.
    for bad in ["soon", "0", "99h", "5x"] {
        beckon(&h)
            .args(["mute", bad])
            .assert()
            .code(2)
            .stderr(contains("duration"));
    }
    beckon(&h)
        .arg("doctor")
        .assert()
        .stdout(contains("muted     no"));
}

/// The whole point of the command: it has to actually silence the hook.
#[test]
fn a_mute_actually_suppresses_a_hook() {
    let h = home();
    let project = tempfile::tempdir().unwrap();
    let trace = h.path().join("trace.log");

    // Isolate mute from the repeat-suppression rule: firing the same state
    // three times in a row would otherwise be throttled on the third, and the
    // test would be measuring the wrong thing.
    beckon(&h)
        .arg("config")
        .args(["set", "policy.rate_limit_ms", "0"])
        .assert()
        .code(0);

    let fire = |trace: &std::path::Path| {
        Command::cargo_bin("beckon")
            .unwrap()
            .args(["hook", "claude-code"])
            .env("BECKON_HOME", h.path())
            .env("BECKON_AUDIO", "null")
            .env("BECKON_TRACE", trace)
            .write_stdin(format!(
                r#"{{"session_id":"s","cwd":"{}","hook_event_name":"Notification","notification_type":"permission_prompt"}}"#,
                project.path().display()
            ))
            .assert()
            .code(0);
    };

    fire(&trace);
    assert!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .contains("play needs-you"),
        "should sound before muting"
    );

    beckon(&h).args(["mute", "1h"]).assert().code(0);
    std::fs::write(&trace, "").unwrap();
    fire(&trace);
    assert!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .contains("suppress muted"),
        "mute did not silence the hook: {}",
        std::fs::read_to_string(&trace).unwrap()
    );

    beckon(&h).arg("unmute").assert().code(0);
    std::fs::write(&trace, "").unwrap();
    fire(&trace);
    assert!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .contains("play needs-you"),
        "unmute did not restore sound"
    );
}

// ── config ────────────────────────────────────────────────────────────────

#[test]
fn config_shows_every_setting() {
    let h = home();
    let out = beckon(&h)
        .arg("config")
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    for key in [
        "pack",
        "volume",
        "policy.min_turn_seconds",
        "events.done",
        "remote.mode",
    ] {
        assert!(text.contains(key), "{key} missing from:\n{text}");
    }
}

#[test]
fn config_set_persists_and_reads_back() {
    let h = home();
    beckon(&h)
        .arg("config")
        .args(["set", "volume", "0.25"])
        .assert()
        .code(0);
    beckon(&h)
        .arg("config")
        .args(["get", "volume"])
        .assert()
        .code(0)
        .stdout(contains("0.25"));
    beckon(&h)
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("volume    0.25"));
}

#[test]
fn config_set_takes_effect_in_the_hook() {
    // Turning an event on must actually change what the hook does.
    let h = home();
    let project = tempfile::tempdir().unwrap();
    let trace = h.path().join("trace.log");
    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"PostToolUseFailure","tool_name":"Bash"}}"#,
        project.path().display()
    );
    let fire = || {
        Command::cargo_bin("beckon")
            .unwrap()
            .args(["hook", "claude-code"])
            .env("BECKON_HOME", h.path())
            .env("BECKON_AUDIO", "null")
            .env("BECKON_TRACE", &trace)
            .write_stdin(payload.clone())
            .assert()
            .code(0);
    };

    fire();
    assert!(std::fs::read_to_string(&trace)
        .unwrap()
        .contains("suppress event-off"));

    beckon(&h)
        .arg("config")
        .args(["set", "events.tool-failed", "true"])
        .assert()
        .code(0);
    std::fs::write(&trace, "").unwrap();
    fire();
    assert!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .contains("play tool-failed"),
        "config change did not reach the hook"
    );
}

#[test]
fn config_set_refuses_an_unknown_key_with_the_valid_list() {
    let h = home();
    beckon(&h)
        .arg("config")
        .args(["set", "volumee", "0.5"])
        .assert()
        .code(2)
        .stderr(contains("volumee"))
        .stderr(contains("policy.min_turn_seconds"));
}

#[test]
fn config_set_refuses_a_value_of_the_wrong_shape() {
    let h = home();
    for (key, bad) in [("volume", "loud"), ("volume", "9"), ("enabled", "maybe")] {
        beckon(&h)
            .arg("config")
            .args(["set", key, bad])
            .assert()
            .code(2)
            .stderr(contains(key));
    }
    beckon(&h)
        .arg("config")
        .args(["get", "volume"])
        .assert()
        .stdout(contains("0.60"));
}

#[test]
fn config_get_rejects_an_unknown_key() {
    let h = home();
    beckon(&h)
        .arg("config")
        .args(["get", "nope"])
        .assert()
        .code(2)
        .stderr(contains("unknown"));
}

#[test]
fn config_path_points_at_the_user_config() {
    let h = home();
    beckon(&h)
        .arg("config")
        .arg("path")
        .assert()
        .code(0)
        .stdout(contains("config.toml"));
}

#[test]
fn config_edits_preserve_a_hand_written_file() {
    let h = home();
    std::fs::write(
        h.path().join("config.toml"),
        "# my notes\npack = \"unit-7\"  # deadpan\n",
    )
    .unwrap();
    beckon(&h)
        .arg("config")
        .args(["set", "volume", "0.4"])
        .assert()
        .code(0);

    let text = std::fs::read_to_string(h.path().join("config.toml")).unwrap();
    assert!(text.contains("# my notes"), "{text}");
    assert!(text.contains("# deadpan"), "{text}");
    beckon(&h)
        .arg("config")
        .args(["get", "pack"])
        .assert()
        .stdout(contains("unit-7"));
}
