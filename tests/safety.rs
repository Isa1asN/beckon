//! The one property beckon must never lose.
//!
//! beckon binds hook events that block the agent on a non-zero exit (`Stop`,
//! `UserPromptSubmit`), and events whose plain stdout is injected into the
//! model's context (`UserPromptSubmit`, `SessionStart`). A sound tool that can
//! wedge a session or poison a prompt is worse than no sound tool at all.

use assert_cmd::Command;

fn hook() -> Command {
    let mut c = Command::cargo_bin("beckon").unwrap();
    c.args(["hook", "claude-code"])
        .env("BECKON_HOME", "/nonexistent/beckon/home");
    c
}

#[test]
fn hook_always_exits_zero_on_garbage_input() {
    let cases: Vec<&[u8]> = vec![
        b"",
        b"not json at all",
        b"{",
        b"{\"hook_event_name\":\"Stop\"",
        b"null",
        b"[]",
        b"{\"hook_event_name\":\"CompletelyUnknownEvent\"}",
        &[0xff, 0xfe, 0x00, 0x01],
    ];
    for case in cases {
        let out = hook().write_stdin(case.to_vec()).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "input {case:?} exited {:?}",
            out.status.code()
        );
        assert!(
            out.stdout.is_empty(),
            "input {case:?} wrote to stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn unknown_agent_exits_zero_silently() {
    Command::cargo_bin("beckon")
        .unwrap()
        .args(["hook", "no-such-agent"])
        .write_stdin("{}")
        .assert()
        .code(0)
        .stdout("");
}

#[test]
fn a_panic_anywhere_still_exits_zero() {
    hook()
        .env("BECKON_PANIC_TEST", "1")
        .write_stdin("{}")
        .assert()
        .code(0)
        .stdout("");
}

#[test]
fn a_malformed_hook_invocation_still_exits_zero() {
    // The guarantee that matters: whatever the agent throws at `hook`, we exit
    // 0. A future agent release passing an argument we do not know must not
    // block the user's session.
    Command::cargo_bin("beckon")
        .unwrap()
        .args(["hook", "claude-code", "--some-future-flag"])
        .write_stdin("{}")
        .assert()
        .code(0)
        .stdout("");
}

#[test]
fn a_person_mistyping_a_subcommand_gets_a_real_exit_code() {
    // Exit 0 is for hooks, not for humans: scripts need to branch on this.
    Command::cargo_bin("beckon")
        .unwrap()
        .arg("no-such-subcommand")
        .assert()
        .code(2)
        .stdout("");
}

#[test]
fn help_and_version_still_succeed() {
    for flag in ["--help", "--version"] {
        Command::cargo_bin("beckon")
            .unwrap()
            .arg(flag)
            .assert()
            .code(0);
    }
}
