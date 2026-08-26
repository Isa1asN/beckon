//! Using your own audio files — and the boundaries around doing so.
//!
//! The security half of this file is the point. Sound file paths arrive from
//! two places with very different trust: your own config, which may name
//! anything, and a pack manifest, which may not name anything outside itself.

use assert_cmd::Command;
use beckon_cli::audio::{synth::Pcm, wav};
// Only the gated decoder tests use `.not()`.
#[cfg(feature = "embedded-audio")]
use predicates::prelude::*;
use predicates::str::contains;
use std::path::PathBuf;

struct Env {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn beckon(&self) -> Command {
        let mut c = Command::cargo_bin("beckon").unwrap();
        c.env("BECKON_HOME", self.home.path())
            .env("BECKON_AUDIO", "null");
        c
    }

    fn user_config(&self, body: &str) {
        std::fs::write(self.home.path().join("config.toml"), body).unwrap();
    }

    fn project_config(&self, body: &str) {
        std::fs::write(self.project.path().join(".beckon.toml"), body).unwrap();
    }

    /// A real, decodable audio file.
    fn make_wav(&self, name: &str, seconds: f32) -> PathBuf {
        let path = self.home.path().join(name);
        let rate = 8_000u32;
        let frames = (rate as f32 * seconds) as usize;
        let samples: Vec<f32> = (0..frames * 2)
            .map(|i| ((i / 2) as f32 * 0.05).sin() * 0.4)
            .collect();
        wav::write(
            &path,
            &Pcm {
                sample_rate: rate,
                channels: 2,
                samples,
            },
        )
        .unwrap();
        path
    }

    /// Fire a hook and wait for the detached player to report back.
    fn fire(&self, event: &str) -> String {
        let trace = self.home.path().join("trace.log");
        let _ = std::fs::remove_file(&trace);

        Command::cargo_bin("beckon")
            .unwrap()
            .args(["hook", "claude-code"])
            .env("BECKON_HOME", self.home.path())
            .env("BECKON_AUDIO", "null")
            .env("BECKON_TRACE", &trace)
            .write_stdin(format!(
                r#"{{"session_id":"s","cwd":"{}",{event}}}"#,
                self.project.path().display()
            ))
            .assert()
            .code(0)
            .stdout("");

        // Playback happens in a detached child, so the last line arrives late.
        for _ in 0..40 {
            let text = std::fs::read_to_string(&trace).unwrap_or_default();
            if text.lines().any(|l| l.starts_with("sound ")) {
                return text;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::fs::read_to_string(&trace).unwrap_or_default()
    }

    fn permission_prompt(&self) -> String {
        r#""hook_event_name":"Notification","notification_type":"permission_prompt""#.to_string()
    }

    /// A pack directory the user "installed", with a chosen sample reference.
    fn install_pack(&self, sample_field: &str) -> PathBuf {
        let dir = self.home.path().join("data/packs/mine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.toml"),
            format!(
                "[pack]\nid=\"mine\"\nname=\"Mine\"\nversion=\"1\"\nauthor=\"a\"\n\
                 license=\"CC0-1.0\"\n\
                 [sounds.done]\ntype=\"synth\"\n\
                 [[sounds.done.layer]]\nwave=\"sine\"\nnotes=[440.0]\n\
                 [sounds.needs-you]\ntype=\"sample\"\nfile=\"{sample_field}\"\n\
                 license=\"CC0-1.0\"\n\
                 [sounds.failed]\ntype=\"synth\"\n\
                 [[sounds.failed.layer]]\nwave=\"saw\"\nnotes=[220.0]\n"
            ),
        )
        .unwrap();
        dir
    }
}

// ── using your own files ──────────────────────────────────────────────────

// Needs a decoder, which exists only with the embedded-audio feature.
#[cfg(feature = "embedded-audio")]
#[test]
fn a_config_override_plays_your_file_through_the_hook() {
    let e = Env::new();
    let mine = e.make_wav("mine.wav", 0.2);
    e.user_config(&format!(
        "[sounds]\nneeds-you = {:?}\n",
        mine.display().to_string()
    ));

    let trace = e.fire(&e.permission_prompt());
    assert!(trace.contains("play needs-you"), "{trace}");
    assert!(
        trace.contains("sound needs-you via null"),
        "the file never played:\n{trace}"
    );
}

#[test]
fn states_you_did_not_override_still_come_from_the_pack() {
    let e = Env::new();
    let mine = e.make_wav("mine.wav", 0.2);
    e.user_config(&format!(
        "[sounds]\nneeds-you = {:?}\n",
        mine.display().to_string()
    ));

    let out = e
        .beckon()
        .args(["test", "--state", "done"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        !text.contains("your file"),
        "done should still be the pack's:\n{text}"
    );
}

#[test]
fn test_shows_which_sounds_are_yours() {
    let e = Env::new();
    let mine = e.make_wav("mine.wav", 0.2);
    e.user_config(&format!(
        "[sounds]\nneeds-you = {:?}\n",
        mine.display().to_string()
    ));

    e.beckon()
        .args(["test", "--state", "needs-you"])
        .assert()
        .code(0)
        .stdout(contains("your file"))
        .stdout(contains("mine.wav"));
}

#[test]
fn an_override_can_voice_a_state_the_pack_leaves_silent() {
    let e = Env::new();
    let mine = e.make_wav("mine.wav", 0.2);
    // aurora defines all nine, so use a pack that does not.
    e.install_pack("mine.wav");
    std::fs::copy(&mine, e.home.path().join("data/packs/mine/mine.wav")).unwrap();
    e.user_config(&format!(
        "pack = \"mine\"\n[sounds]\ncompacting = {:?}\n",
        mine.display().to_string()
    ));

    e.beckon()
        .args(["test", "mine", "--state", "compacting"])
        .assert()
        .code(0)
        .stdout(contains("your file"));
}

#[test]
fn a_broken_override_costs_one_chime_and_nothing_else() {
    let e = Env::new();
    e.user_config("[sounds]\nneeds-you = \"/nonexistent/gone.wav\"\n");

    let trace = e.fire(&e.permission_prompt());
    assert!(
        trace.contains("play needs-you"),
        "the decision should still be made:\n{trace}"
    );
    assert!(
        trace.contains("sound skipped"),
        "should explain itself:\n{trace}"
    );
}

#[test]
fn doctor_reports_a_broken_override_rather_than_going_quiet() {
    let e = Env::new();
    e.user_config("[sounds]\ndone = \"/nonexistent/gone.wav\"\n");
    e.beckon()
        .arg("doctor")
        .assert()
        .code(0)
        .stdout(contains("BROKEN"));
}

// Needs a decoder, which exists only with the embedded-audio feature.
#[cfg(feature = "embedded-audio")]
#[test]
fn a_pack_sample_that_lives_inside_the_pack_plays() {
    let e = Env::new();
    let dir = e.install_pack("alert.wav");
    let source = e.make_wav("source.wav", 0.2);
    std::fs::copy(&source, dir.join("alert.wav")).unwrap();

    e.beckon()
        .args(["test", "mine", "--state", "needs-you"])
        .assert()
        .code(0)
        .stdout(contains("needs-you"))
        .stdout(contains("could not be loaded").not());
}

// ── security: a project may not name files ────────────────────────────────

#[test]
fn a_cloned_repository_cannot_point_beckon_at_your_files() {
    // The boundary that matters: .beckon.toml arrives with the repository.
    let e = Env::new();
    let secret = e.make_wav("secret.wav", 0.2);
    e.project_config(&format!(
        "[sounds]\nneeds-you = {:?}\n",
        secret.display().to_string()
    ));

    let trace = e.fire(&e.permission_prompt());
    assert!(
        trace.contains("ignoring [sounds]"),
        "the refusal must be visible in the trace:\n{trace}"
    );

    // The pack's own needs-you still plays — that is correct. What must not
    // happen is the *project's* file being used, so check the source directly.
    let out = e
        .beckon()
        .args(["test", "--state", "needs-you"])
        .current_dir(e.project.path())
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        !text.contains("your file"),
        "the project's file was used:\n{text}"
    );
    assert!(
        !text.contains("secret.wav"),
        "the project's file was used:\n{text}"
    );
}

#[test]
fn a_project_config_cannot_override_a_sound_you_chose() {
    let e = Env::new();
    let mine = e.make_wav("mine.wav", 0.2);
    let theirs = e.make_wav("theirs.wav", 0.2);
    e.user_config(&format!(
        "[sounds]\nneeds-you = {:?}\n",
        mine.display().to_string()
    ));
    e.project_config(&format!(
        "[sounds]\nneeds-you = {:?}\n",
        theirs.display().to_string()
    ));

    let out = e
        .beckon()
        .args(["test", "--state", "needs-you"])
        .current_dir(e.project.path())
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("mine.wav"), "{text}");
    assert!(
        !text.contains("theirs.wav"),
        "the project overrode your choice:\n{text}"
    );
}

// ── security: a pack may not escape itself ────────────────────────────────

#[test]
fn a_pack_cannot_read_a_file_outside_itself_by_traversal() {
    let e = Env::new();
    e.install_pack("../../../../etc/passwd");
    e.user_config("pack = \"mine\"\n");

    let out = e
        .beckon()
        .args(["test", "mine", "--state", "needs-you"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("could not be loaded"),
        "traversal was not refused:\n{text}"
    );
}

#[test]
fn a_pack_cannot_name_an_absolute_path() {
    let e = Env::new();
    let outside = e.make_wav("outside.wav", 0.2);
    e.install_pack(&outside.display().to_string());
    e.user_config("pack = \"mine\"\n");

    let out = e
        .beckon()
        .args(["test", "mine", "--state", "needs-you"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("could not be loaded"),
        "absolute path was allowed:\n{text}"
    );
}

#[cfg(unix)]
#[test]
fn a_pack_cannot_escape_through_a_symlink() {
    let e = Env::new();
    let dir = e.install_pack("alert.wav");
    let outside = e.make_wav("outside.wav", 0.2);
    std::os::unix::fs::symlink(&outside, dir.join("alert.wav")).unwrap();
    e.user_config("pack = \"mine\"\n");

    let out = e
        .beckon()
        .args(["test", "mine", "--state", "needs-you"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("could not be loaded"),
        "symlink escape was allowed:\n{text}"
    );
}

#[test]
fn a_pack_cannot_reference_a_non_audio_file() {
    let e = Env::new();
    let dir = e.install_pack("payload.sh");
    std::fs::write(dir.join("payload.sh"), "#!/bin/sh\necho pwned\n").unwrap();
    e.user_config("pack = \"mine\"\n");

    let out = e
        .beckon()
        .args(["test", "mine", "--state", "needs-you"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("could not be loaded"),
        "a .sh was accepted:\n{text}"
    );
}

#[test]
fn nothing_in_a_hostile_pack_can_make_the_hook_fail() {
    // Whatever a pack does, the agent must not be affected.
    let e = Env::new();
    e.install_pack("../../../../etc/passwd");
    e.user_config("pack = \"mine\"\n");
    let trace = e.fire(&e.permission_prompt());
    assert!(trace.contains("play needs-you"), "{trace}");
}

// ── security: config set validates before it writes ───────────────────────

#[test]
fn setting_a_sound_to_something_unreadable_is_refused() {
    let e = Env::new();
    for bad in ["/nonexistent/gone.wav", "/etc/passwd", "/dev/zero", "/tmp"] {
        e.beckon()
            .arg("config")
            .args(["set", "sounds.done", bad])
            .assert()
            .code(2)
            .stderr(contains("readable audio file"));
    }
    assert!(
        !e.home.path().join("config.toml").exists(),
        "a refused set must not create a config"
    );
}

// Needs a decoder, which exists only with the embedded-audio feature.
#[cfg(feature = "embedded-audio")]
#[test]
fn setting_a_sound_to_a_real_file_works_and_reads_back() {
    let e = Env::new();
    let mine = e.make_wav("mine.wav", 0.2);
    e.beckon()
        .arg("config")
        .args(["set", "sounds.done", &mine.display().to_string()])
        .assert()
        .code(0);
    e.beckon()
        .arg("config")
        .args(["get", "sounds.done"])
        .assert()
        .code(0)
        .stdout(contains("mine.wav"));
}

#[test]
fn a_sound_path_is_not_executed_merely_by_being_configured() {
    // Belt and braces: prove no shell interpretation of the path happens.
    let e = Env::new();
    let marker = e.home.path().join("ran");
    let hostile = format!("/nonexistent/$(touch {}).wav", marker.display());
    e.beckon()
        .arg("config")
        .args(["set", "sounds.done", &hostile])
        .assert()
        .code(2);
    assert!(!marker.exists(), "the path was evaluated by a shell");
}
