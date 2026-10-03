//! `beckon init` and `beckon uninstall`.
//!
//! These edit the file that decides what the agent may execute, so the tests
//! are about restraint: touch exactly our own entries, never anything else, and
//! never write without a backup.

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;

struct Env {
    _home: tempfile::TempDir,
    claude: tempfile::TempDir,
}

const FOREIGN: &str = r#"{
  "model": "opus",
  "permissions": { "allow": ["Bash(git status)"] },
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "/usr/bin/notify-send done" }] }
    ],
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "./guard.sh" }] }
    ]
  }
}"#;

impl Env {
    fn new() -> Env {
        Env {
            _home: tempfile::tempdir().unwrap(),
            claude: tempfile::tempdir().unwrap(),
        }
    }

    fn beckon(&self) -> Command {
        let mut c = Command::cargo_bin("beckon").unwrap();
        c.env("CLAUDE_CONFIG_DIR", self.claude.path())
            .env("BECKON_HOME", self._home.path())
            .env("BECKON_AUDIO", "null");
        c
    }

    fn settings_path(&self) -> std::path::PathBuf {
        self.claude.path().join("settings.json")
    }

    fn write_settings(&self, text: &str) {
        std::fs::write(self.settings_path(), text).unwrap();
    }

    fn read_settings(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.settings_path()).unwrap()).unwrap()
    }

    fn backups(&self) -> Vec<String> {
        std::fs::read_dir(self.claude.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("beckon-backup"))
            .collect()
    }
}

/// The program part of a hook command, unquoted.
fn program_of(command: &str) -> &str {
    match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command.split_whitespace().next().unwrap_or(command),
    }
}

fn commands_for(settings: &Value, event: &str) -> Vec<String> {
    settings["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn init_binds_all_nine_events() {
    let e = Env::new();
    e.beckon()
        .args(["init", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("installed"));

    let settings = e.read_settings();
    for event in [
        "UserPromptSubmit",
        "Stop",
        "Notification",
        "StopFailure",
        "PostToolUseFailure",
        "SubagentStop",
        "PreCompact",
        "SessionStart",
        "SessionEnd",
    ] {
        let commands = commands_for(&settings, event);
        assert!(
            commands
                .iter()
                .any(|c| c.contains("beckon") && c.contains("hook claude-code")),
            "{event} not bound: {commands:?}"
        );
    }
}

#[test]
fn init_writes_an_absolute_command_so_a_minimal_path_still_works() {
    let e = Env::new();
    e.beckon().args(["init", "--yes"]).assert().code(0);
    let command = commands_for(&e.read_settings(), "Stop")[0].clone();
    assert!(
        std::path::Path::new(program_of(&command)).is_absolute(),
        "not absolute: {command}"
    );
}

#[test]
fn init_leaves_foreign_hooks_and_settings_alone() {
    let e = Env::new();
    e.write_settings(FOREIGN);
    e.beckon()
        .args(["init", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("Left untouched"));

    let settings = e.read_settings();
    assert_eq!(settings["model"], "opus");
    assert_eq!(settings["permissions"]["allow"][0], "Bash(git status)");
    assert!(commands_for(&settings, "PreToolUse")
        .iter()
        .any(|c| c.contains("guard.sh")));
    assert!(commands_for(&settings, "Stop")
        .iter()
        .any(|c| c.contains("notify-send")));
}

#[test]
fn init_is_idempotent() {
    // Start from an existing file, so the first run has something to back up
    // and the second can be shown to add nothing at all.
    let e = Env::new();
    e.write_settings(FOREIGN);

    e.beckon().args(["init", "--yes"]).assert().code(0);
    let after_first = e.read_settings();
    assert_eq!(e.backups().len(), 1);

    e.beckon()
        .args(["init", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("Already installed"));

    assert_eq!(
        e.read_settings(),
        after_first,
        "a second init must change nothing"
    );
    assert_eq!(e.backups().len(), 1, "a no-op must not pile up backups");
}

#[test]
fn init_on_a_fresh_machine_has_nothing_to_back_up() {
    let e = Env::new();
    e.beckon().args(["init", "--yes"]).assert().code(0);
    assert!(e.settings_path().exists());
    assert!(e.backups().is_empty(), "there was no file to preserve");
}

#[test]
fn a_dry_run_writes_nothing() {
    let e = Env::new();
    e.write_settings(FOREIGN);
    e.beckon()
        .args(["init", "--dry-run"])
        .assert()
        .code(0)
        .stdout(contains("Dry run"))
        // Not "beckon hook …": the program is `beckon.exe` on Windows.
        .stdout(contains("hook claude-code"));

    assert_eq!(std::fs::read_to_string(e.settings_path()).unwrap(), FOREIGN);
    assert!(e.backups().is_empty());
}

#[test]
fn the_diff_is_shown_with_context_before_writing() {
    let e = Env::new();
    e.write_settings(FOREIGN);
    let out = e
        .beckon()
        .args(["init", "--dry-run"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.lines().any(|l| l.trim_start().starts_with('+')),
        "no additions:\n{text}"
    );
    assert!(
        text.contains("notify-send"),
        "context should show the neighbours:\n{text}"
    );
}

#[test]
fn uninstall_restores_the_file_exactly() {
    let e = Env::new();
    e.write_settings(FOREIGN);
    let original: Value = serde_json::from_str(FOREIGN).unwrap();

    e.beckon().args(["init", "--yes"]).assert().code(0);
    assert_ne!(e.read_settings(), original);

    e.beckon()
        .args(["uninstall", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("removed"));
    assert_eq!(
        e.read_settings(),
        original,
        "uninstall must restore the document exactly"
    );
}

#[test]
fn uninstall_on_a_file_without_our_hooks_changes_nothing() {
    let e = Env::new();
    e.write_settings(FOREIGN);
    e.beckon()
        .args(["uninstall", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("No beckon hooks found"));
    assert_eq!(std::fs::read_to_string(e.settings_path()).unwrap(), FOREIGN);
}

#[test]
fn uninstall_with_no_settings_file_is_a_no_op() {
    let e = Env::new();
    e.beckon()
        .args(["uninstall", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("Nothing to remove"));
}

#[test]
fn every_write_leaves_a_distinct_backup() {
    let e = Env::new();
    e.write_settings(FOREIGN);
    e.beckon().args(["init", "--yes"]).assert().code(0);
    e.beckon().args(["uninstall", "--yes"]).assert().code(0);

    let backups = e.backups();
    assert_eq!(
        backups.len(),
        2,
        "back-to-back writes must not share a name: {backups:?}"
    );

    // The earliest backup must be the pristine original.
    let mut sorted = backups.clone();
    sorted.sort();
    let first = std::fs::read_to_string(e.claude.path().join(&sorted[0])).unwrap();
    assert_eq!(first, FOREIGN);
}

#[test]
fn malformed_settings_are_refused_rather_than_overwritten() {
    // Someone's hand-edited file with a trailing comma is not ours to "fix".
    let e = Env::new();
    let broken = "{ \"model\": \"opus\", }";
    e.write_settings(broken);

    e.beckon()
        .args(["init", "--yes"])
        .assert()
        .code(1)
        .stderr(contains("not valid JSON"))
        .stderr(contains("Refusing"));

    assert_eq!(std::fs::read_to_string(e.settings_path()).unwrap(), broken);
    assert!(e.backups().is_empty());
}

#[test]
fn without_a_terminal_confirmation_must_be_explicit() {
    let e = Env::new();
    e.beckon()
        .arg("init")
        .assert()
        .code(1)
        .stderr(contains("--yes"))
        .stderr(contains("--dry-run"));
    assert!(
        !e.settings_path().exists(),
        "nothing should have been written"
    );
}

#[test]
fn an_unknown_agent_is_reported_clearly() {
    let e = Env::new();
    e.beckon()
        .args(["init", "--agent", "emacs", "--yes"])
        .assert()
        .code(1)
        .stderr(contains("unknown agent"));
}

#[test]
fn project_scope_writes_into_the_repository() {
    let e = Env::new();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();

    e.beckon()
        .args(["init", "--scope", "project", "--yes"])
        .current_dir(project.path())
        .assert()
        .code(0);

    assert!(project.path().join(".claude/settings.json").exists());
    assert!(
        !e.settings_path().exists(),
        "user settings must be untouched"
    );
}

// ── regressions: shapes that used to be destroyed ─────────────────────────
//
// Every case here was reproduced against a real binary before it was fixed.
// They are the reason `merge` leaves unfamiliar shapes alone rather than
// normalising them.

#[test]
fn a_utf16_settings_file_is_refused_not_replaced() {
    // What Windows Notepad and PowerShell 5.1 produce. This used to be read as
    // "empty", so the user's whole config was replaced by beckon's hooks — and
    // the preview showed it as having been empty all along.
    let e = Env::new();
    let utf16: Vec<u8> = {
        let mut bytes = vec![0xff, 0xfe];
        for unit in FOREIGN.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    };
    std::fs::write(e.settings_path(), &utf16).unwrap();

    e.beckon()
        .args(["init", "--yes"])
        .assert()
        .code(1)
        .stderr(contains("UTF-8"));
    assert_eq!(
        std::fs::read(e.settings_path()).unwrap(),
        utf16,
        "the file was modified"
    );
}

#[test]
fn a_top_level_non_object_is_refused() {
    for body in ["[1, 2, 3]", "\"a string\"", "42", "true"] {
        let e = Env::new();
        e.write_settings(body);
        e.beckon().args(["init", "--yes"]).assert().code(1);
        assert_eq!(std::fs::read_to_string(e.settings_path()).unwrap(), body);
    }
}

#[test]
fn a_hooks_key_of_an_unexpected_type_is_refused() {
    for body in [
        r#"{"hooks": "see hooks.d/"}"#,
        r#"{"hooks": 42}"#,
        r#"{"hooks": [{"event": "Stop"}]}"#,
    ] {
        let e = Env::new();
        e.write_settings(body);
        e.beckon()
            .args(["init", "--yes"])
            .assert()
            .code(1)
            .stderr(contains("does not recognise"));
        assert_eq!(std::fs::read_to_string(e.settings_path()).unwrap(), body);
    }
}

#[test]
fn hook_groups_in_shapes_beckon_does_not_write_are_left_alone() {
    // `strip_beckon_from_group` used to return "drop this" for anything it
    // could not destructure, so init silently deleted the neighbours below
    // while reporting "No other hooks in this file".
    for (label, body) in [
        (
            "legacy flat",
            r#"{"hooks":{"Stop":[{"matcher":"Bash","command":"legacy.sh"}]}}"#,
        ),
        (
            "empty hooks array",
            r#"{"hooks":{"Stop":[{"matcher":"Bash","hooks":[]}]}}"#,
        ),
        (
            "hooks as object",
            r#"{"hooks":{"Stop":[{"hooks":{"command":"mine.sh"}}]}}"#,
        ),
        ("scalar group", r#"{"hooks":{"Stop":["./bare.sh"]}}"#),
    ] {
        let e = Env::new();
        e.write_settings(body);
        e.beckon().args(["init", "--yes"]).assert().code(0);

        let after = std::fs::read_to_string(e.settings_path()).unwrap();
        for needle in ["legacy.sh", "mine.sh", "bare.sh", "matcher"] {
            if body.contains(needle) {
                assert!(after.contains(needle), "{label}: lost {needle}\n{after}");
            }
        }
    }
}

#[test]
fn uninstall_never_touches_events_beckon_does_not_bind() {
    // This fired on a file beckon had never run against.
    let e = Env::new();
    let body = r#"{"hooks":{"PostToolUse":[{"matcher":"Bash"}],"Stop":[]}}"#;
    e.write_settings(body);
    e.beckon()
        .args(["uninstall", "--yes"])
        .assert()
        .code(0)
        .stdout(contains("No beckon hooks found"));
    assert_eq!(std::fs::read_to_string(e.settings_path()).unwrap(), body);
}

#[test]
fn a_path_with_spaces_is_still_recognised_as_our_own() {
    // The command is quoted when the path has spaces; matching split on
    // whitespace first, so beckon stopped recognising itself — every init
    // appended another copy and uninstall became a no-op.
    let e = Env::new();
    let dir = e._home.path().join("my bin");
    std::fs::create_dir_all(&dir).unwrap();
    // Keep the original file name: on Windows a copy without `.exe` cannot be
    // executed at all.
    let source = assert_cmd::cargo::cargo_bin("beckon");
    let exe = dir.join(source.file_name().unwrap());

    // Retry: cargo may still be writing target/debug/beckon when a sibling test
    // binary starts, and copying a file being written fails with ETXTBSY.
    // Copy, then wait until the copy is actually executable.
    //
    // `cargo test` runs suites in threads, and a sibling spawning a process
    // while this copy is open can leave the descriptor held in its child — the
    // exec then fails with ETXTBSY. Both steps are retried because either can
    // lose the race.
    let mut ready = false;
    for _ in 0..40 {
        if std::fs::copy(&source, &exe).is_ok()
            && std::process::Command::new(&exe)
                .arg("--version")
                .output()
                .is_ok()
        {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        ready,
        "could not run a copy of the binary from a path containing a space"
    );

    let run = |args: &[&str]| {
        Command::new(&exe)
            .args(args)
            .env("CLAUDE_CONFIG_DIR", e.claude.path())
            .env("BECKON_HOME", e._home.path())
            .assert()
            .code(0);
    };

    run(&["init", "--yes"]);
    run(&["init", "--yes"]);
    run(&["init", "--yes"]);
    assert_eq!(
        commands_for(&e.read_settings(), "Stop").len(),
        1,
        "duplicated"
    );

    run(&["uninstall", "--yes"]);
    assert!(
        commands_for(&e.read_settings(), "Stop").is_empty(),
        "could not remove its own entry"
    );
}

#[test]
fn a_command_someone_has_edited_is_neither_removed_nor_duplicated() {
    // Appending `&& notify-send` makes the line theirs. Deleting it loses their
    // work; adding a second beckon entry beside it is noise.
    let e = Env::new();
    e.write_settings(
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command",
           "command":"/usr/local/bin/beckon hook claude-code && notify-send done"}]}]}}"#,
    );
    e.beckon().args(["init", "--yes"]).assert().code(0);

    let commands = commands_for(&e.read_settings(), "Stop");
    assert_eq!(
        commands.len(),
        1,
        "should not have added a second entry: {commands:?}"
    );
    assert!(
        commands[0].contains("notify-send"),
        "their edit was discarded"
    );
}

#[test]
fn a_script_merely_named_like_beckon_is_left_alone() {
    // `beckon.sh` and `beckon.py` are somebody else's. File-stem matching swept
    // them away.
    let e = Env::new();
    e.write_settings(
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/home/u/beckon.sh hook mine"}]}]}}"#,
    );
    e.beckon().args(["init", "--yes"]).assert().code(0);
    let commands = commands_for(&e.read_settings(), "Stop");
    assert!(
        commands.iter().any(|c| c.contains("beckon.sh")),
        "deleted: {commands:?}"
    );
    assert_eq!(commands.len(), 2, "ours should have been added alongside");
}

#[test]
fn project_scope_refuses_when_there_is_no_repository() {
    // It used to fall back to whatever marker it found walking up — including
    // a dotfiles repo at $HOME, which meant writing the user-scope file.
    let e = Env::new();
    let loose = tempfile::tempdir().unwrap();
    e.beckon()
        .args(["init", "--scope", "project", "--yes"])
        .current_dir(loose.path())
        .assert()
        .code(1)
        .stderr(contains("not inside a repository"));
}

#[test]
fn a_symlinked_settings_file_stays_a_symlink() {
    // The dotfiles pattern: ~/.claude/settings.json -> ~/dotfiles/…
    // Replacing the link with a regular file severs it silently.
    #[cfg(unix)]
    {
        let e = Env::new();
        let real_dir = tempfile::tempdir().unwrap();
        let real = real_dir.path().join("settings.json");
        std::fs::write(&real, FOREIGN).unwrap();
        std::os::unix::fs::symlink(&real, e.settings_path()).unwrap();

        e.beckon().args(["init", "--yes"]).assert().code(0);

        assert!(
            std::fs::symlink_metadata(e.settings_path())
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink was replaced by a regular file"
        );
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
        assert!(
            !commands_for(&written, "Stop").is_empty(),
            "the edit did not reach the symlink target"
        );
    }
}

// ------------------------------------------------------------------- doctor

impl Env {
    /// `doctor`'s output, run from outside any repository so only the
    /// user-scope file this test controls can be reported.
    fn doctor(&self) -> String {
        let out = self
            .beckon()
            .arg("doctor")
            .current_dir(self._home.path())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn doctor_says_hooks_are_unbound_only_when_they_are() {
    // It used to say "run `beckon init`" unconditionally — before and after.
    let e = Env::new();
    e.write_settings(FOREIGN);
    assert!(e
        .doctor()
        .contains("hooks     not bound — run `beckon init`"));

    e.beckon().args(["init", "--yes"]).assert().code(0);
    let report = e.doctor();
    assert!(report.contains("hooks     all 9 bound in"), "{report}");
    assert!(report.contains("this binary"), "{report}");
    assert!(!report.contains("not bound"), "{report}");
}

#[test]
fn doctor_names_the_events_a_partial_install_is_missing() {
    let e = Env::new();
    e.beckon().args(["init", "--yes"]).assert().code(0);
    let mut settings = e.read_settings();
    let hooks = settings["hooks"].as_object_mut().unwrap();
    hooks.remove("Stop");
    hooks.remove("PreCompact");
    e.write_settings(&settings.to_string());

    let report = e.doctor();
    assert!(report.contains("7 of 9 bound"), "{report}");
    assert!(report.contains("missing Stop, PreCompact"), "{report}");
}

#[test]
fn doctor_flags_a_hook_whose_beckon_no_longer_exists() {
    // Uninstalled or moved binary: every hook fails and every sound is lost,
    // which is the silence that is hardest to diagnose from the outside.
    // Absolute on every platform: `/nowhere/...` has no drive on Windows, so
    // it is not absolute there and doctor rightly declines to guess.
    let e = Env::new();
    let gone = e._home.path().join("uninstalled").join("beckon");
    let command = format!("\"{}\" hook claude-code", gone.display());
    e.write_settings(
        &serde_json::json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": command}]}]}})
            .to_string(),
    );
    let report = e.doctor();
    assert!(report.contains("1 of 9 bound"), "{report}");
    assert!(report.contains("NOT FOUND"), "{report}");
}

#[cfg(windows)]
#[test]
fn doctor_does_not_call_a_driveless_windows_path_missing() {
    // `/tools/beckon` lands on whichever drive the agent runs from, or under
    // Git Bash's root — not knowable from here, and not the same as missing.
    let e = Env::new();
    e.write_settings(
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/tools/beckon hook claude-code"}]}]}}"#,
    );
    let report = e.doctor();
    assert!(
        report.contains("cannot tell from here: a path with no drive"),
        "{report}"
    );
    assert!(!report.contains("NOT FOUND"), "{report}");
}

#[cfg(unix)]
fn executable(path: &std::path::Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn doctor_does_not_guess_at_paths_only_the_hook_shell_can_resolve() {
    // A wrong "NOT FOUND" sends someone chasing a hook that works.
    let e = Env::new();
    e.write_settings(
        r#"{"hooks":{
            "Stop":[{"hooks":[{"type":"command","command":"$HOME/bin/beckon hook claude-code"}]}],
            "Notification":[{"hooks":[{"type":"command","command":"./beckon hook claude-code"}]}]
        }}"#,
    );
    let report = e.doctor();
    assert!(
        report.contains("cannot tell from here: it uses shell expansion"),
        "{report}"
    );
    assert!(
        report.contains("cannot tell from here: a relative path"),
        "{report}"
    );
    assert!(!report.contains("NOT FOUND"), "{report}");
}

#[cfg(unix)]
#[test]
fn doctor_expands_a_tilde_path_as_the_hook_shell_would() {
    let e = Env::new();
    let home = tempfile::tempdir().unwrap();
    let ours = assert_cmd::cargo::cargo_bin("beckon");
    std::fs::create_dir_all(home.path().join("bin")).unwrap();
    std::os::unix::fs::symlink(&ours, home.path().join("bin/beckon")).unwrap();
    e.write_settings(
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"~/bin/beckon hook claude-code"}]}]}}"#,
    );
    let out = e
        .beckon()
        .arg("doctor")
        .env("HOME", home.path())
        .current_dir(e._home.path())
        .output()
        .unwrap();
    let report = String::from_utf8(out.stdout).unwrap();
    assert!(report.contains("this binary"), "{report}");
}

#[cfg(unix)]
#[test]
fn doctor_never_runs_a_program_named_by_a_repositorys_settings() {
    // A cloned repository can commit .claude/settings.json. Claude Code will
    // not run its hooks until the workspace is trusted; doctor must not run
    // them at all.
    let e = Env::new();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let marker = repo.path().join("ran");
    let fake = repo.path().join("tools/beckon");
    executable(
        &fake,
        &format!(
            "#!/bin/sh\ntouch '{}'\necho beckon 9.9.9\n",
            marker.display()
        ),
    );
    std::fs::create_dir(repo.path().join(".claude")).unwrap();
    std::fs::write(
        repo.path().join(".claude/settings.json"),
        format!(
            r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{} hook claude-code"}}]}}]}}}}"#,
            fake.display()
        ),
    )
    .unwrap();

    let out = e
        .beckon()
        .arg("doctor")
        .current_dir(repo.path())
        .output()
        .unwrap();
    let report = String::from_utf8(out.stdout).unwrap();
    assert!(
        !marker.exists(),
        "doctor executed the repository's program:\n{report}"
    );
    assert!(report.contains("not run to ask its version"), "{report}");
}

#[cfg(unix)]
#[test]
fn doctor_gives_up_on_a_wrapper_that_never_closes_its_output() {
    // The wrapper exits at once, but leaves a child holding stdout open. A
    // read waiting for end of output would hang doctor for as long as it runs.
    let e = Env::new();
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("beckon");
    executable(&fake, "#!/bin/sh\nsleep 30 &\nexit 0\n");
    e.write_settings(&format!(
        r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{} hook claude-code"}}]}}]}}}}"#,
        fake.display()
    ));
    let started = std::time::Instant::now();
    let report = e.doctor();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "doctor took {:?}",
        started.elapsed()
    );
    assert!(report.contains("did not report a version"), "{report}");
}

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn a_private_settings_file_stays_private_through_init_and_uninstall() {
    // settings.json holds API keys and MCP tokens. Rewriting it through a temp
    // file created with the default umask used to leave it 0664.
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.write_settings(r#"{"env":{"ANTHROPIC_API_KEY":"sk-not-real"}}"#);
    std::fs::set_permissions(e.settings_path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    e.beckon().args(["init", "--yes"]).assert().code(0);
    assert_eq!(
        mode_of(&e.settings_path()),
        0o600,
        "init loosened settings.json"
    );
    for backup in e.backups() {
        assert_eq!(
            mode_of(&e.claude.path().join(&backup)),
            0o600,
            "{backup} is not private"
        );
    }

    e.beckon().args(["uninstall", "--yes"]).assert().code(0);
    assert_eq!(
        mode_of(&e.settings_path()),
        0o600,
        "uninstall loosened settings.json"
    );
}

#[cfg(unix)]
#[test]
fn a_settings_file_keeps_whatever_mode_it_had() {
    // Not tightened either: someone who shares it on purpose keeps sharing it.
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.write_settings("{}");
    std::fs::set_permissions(e.settings_path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    e.beckon().args(["init", "--yes"]).assert().code(0);
    assert_eq!(mode_of(&e.settings_path()), 0o644);
}

#[cfg(unix)]
#[test]
fn a_settings_file_init_creates_is_private() {
    let e = Env::new();
    e.beckon().args(["init", "--yes"]).assert().code(0);
    assert_eq!(mode_of(&e.settings_path()), 0o600);
}

/// Run a binary this test just copied. Exec can fail with ETXTBSY when another
/// test thread forks while the copy is still open for writing; that one error
/// is retried, anything else is a real failure.
#[cfg(unix)]
fn output_of_copied(command: &mut std::process::Command) -> std::process::Output {
    for _ in 0..100 {
        match command.output() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            other => return other.unwrap(),
        }
    }
    panic!("binary stayed busy")
}

#[cfg(unix)]
#[test]
fn a_hostile_install_path_is_quoted_so_the_hook_runs_exactly_beckon() {
    // The hook command goes through a shell. An install path containing a
    // quote used to leave it unterminated (exit 2: a blocking error on every
    // prompt), and `$(...)` or `;` in it ran as commands.
    let ours = assert_cmd::cargo::cargo_bin("beckon");
    for name in [
        "o'brien",
        "semi;touch PWNED_SEMI;x",
        "$(touch PWNED_SUBST)",
        "sp `touch PWNED_BT`",
    ] {
        let e = Env::new();
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join(name);
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("beckon");
        std::fs::copy(&ours, &bin).unwrap();

        let out = output_of_copied(
            std::process::Command::new(&bin)
                .args(["init", "--yes"])
                .env("CLAUDE_CONFIG_DIR", e.claude.path())
                .env("BECKON_HOME", e._home.path()),
        );
        assert_eq!(out.status.code(), Some(0), "{name}: {out:?}");
        let command = commands_for(&e.read_settings(), "Stop").remove(0);

        // Run it the way the agent does, from a scratch directory.
        let cwd = tempfile::tempdir().unwrap();
        let trace = cwd.path().join("trace.log");
        let mut child = std::process::Command::new("sh")
            .args(["-c", &command])
            .current_dir(cwd.path())
            .env("BECKON_HOME", e._home.path())
            .env("BECKON_AUDIO", "null")
            .env("BECKON_TRACE", &trace)
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s","hook_event_name":"Stop"}"#)
            .unwrap();
        let status = child.wait().unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "{name}: `{command}` exited {status}"
        );
        let traced = std::fs::read_to_string(&trace).unwrap_or_default();
        assert!(
            traced.contains("play done"),
            "{name}: `{command}` did not run beckon: {traced}"
        );
        let created: Vec<_> = std::fs::read_dir(cwd.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("PWNED"))
            .collect();
        assert!(
            created.is_empty(),
            "{name}: the path ran as a command: {created:?}"
        );

        // And uninstall still recognises what init wrote.
        let out = output_of_copied(
            std::process::Command::new(&bin)
                .args(["uninstall", "--yes"])
                .env("CLAUDE_CONFIG_DIR", e.claude.path())
                .env("BECKON_HOME", e._home.path()),
        );
        assert_eq!(out.status.code(), Some(0));
        assert!(
            commands_for(&e.read_settings(), "Stop").is_empty(),
            "{name}: uninstall missed it"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dangling_settings_symlink_is_refused_rather_than_replaced() {
    // A dotfiles link whose target is not checked out. Read as empty, it would
    // be overwritten with a regular file and the link cut.
    let e = Env::new();
    std::os::unix::fs::symlink(
        e.claude.path().join("not-checked-out.json"),
        e.settings_path(),
    )
    .unwrap();
    e.beckon()
        .args(["init", "--yes"])
        .assert()
        .code(1)
        .stderr(contains("symlink to a file that does not exist"));
    assert!(std::fs::symlink_metadata(e.settings_path())
        .unwrap()
        .file_type()
        .is_symlink());
}
