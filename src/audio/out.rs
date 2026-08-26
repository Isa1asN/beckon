//! Getting samples to a speaker.
//!
//! Three tiers, first success wins:
//!
//! 1. **Embedded** — in-process via rodio/cpal. Works on a machine with no
//!    audio tools installed at all, which is the most common Linux support
//!    burden for tools like this.
//! 2. **System player** — render to a temp WAV and shell out. Covers static
//!    musl builds compiled without the `embedded-audio` feature, and machines
//!    where cpal cannot open a device.
//! 3. **Terminal bell** — `\x07`. Something beats nothing.
//!
//! `beckon doctor` reports which tier is live and why the others were skipped,
//! so a silent tool is never a mystery.

use crate::audio::synth::Pcm;
use crate::audio::wav;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// In-process, via rodio.
    Embedded,
    /// Shelled out to this program.
    System(&'static str),
    /// Terminal bell.
    Bell,
    /// Deliberately silent — tests, `--dry-run`, and `BECKON_AUDIO=null`.
    Null,
}

impl Backend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Backend::Embedded => "embedded",
            Backend::System(program) => program,
            Backend::Bell => "bell",
            Backend::Null => "null",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backend::System(program) => f.pad(&format!("system player ({program})")),
            other => f.pad(other.as_str()),
        }
    }
}

/// System players worth trying, best first.
#[cfg(target_os = "macos")]
const SYSTEM_PLAYERS: &[(&str, &[&str])] = &[("afplay", &[])];

#[cfg(target_os = "windows")]
const SYSTEM_PLAYERS: &[(&str, &[&str])] = &[("powershell", &["-NoProfile", "-Command"])];

#[cfg(all(unix, not(target_os = "macos")))]
const SYSTEM_PLAYERS: &[(&str, &[&str])] = &[
    ("paplay", &[]),
    ("pw-play", &[]),
    ("aplay", &["-q"]),
    ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
    ("cvlc", &["--play-and-exit", "--intf", "dummy"]),
];

/// Play, choosing a backend automatically. Returns what actually ran.
pub fn play(pcm: &Pcm, volume: f32) -> Backend {
    play_with(pcm, volume, override_from_env())
}

/// Play, optionally forcing a backend.
///
/// A forced backend is honoured exactly — no fallback — so a test asking for
/// silence cannot end up making noise, and `doctor` can probe one tier at a
/// time.
pub fn play_with(pcm: &Pcm, volume: f32, forced: Option<Backend>) -> Backend {
    if let Some(backend) = forced {
        run(pcm, volume, backend);
        return backend;
    }

    #[cfg(feature = "embedded-audio")]
    if play_embedded(pcm, volume).is_ok() {
        return Backend::Embedded;
    }

    if let Some(program) = available_system_player() {
        if play_system(pcm, volume, program).is_ok() {
            return Backend::System(program);
        }
    }

    ring_bell();
    Backend::Bell
}

fn run(pcm: &Pcm, volume: f32, backend: Backend) {
    match backend {
        Backend::Null => {}
        Backend::Bell => ring_bell(),
        Backend::System(program) => {
            let _ = play_system(pcm, volume, program);
        }
        Backend::Embedded => {
            #[cfg(feature = "embedded-audio")]
            let _ = play_embedded(pcm, volume);
        }
    }
}

/// `BECKON_AUDIO` forces a backend. Mostly for tests and for debugging a
/// machine where the automatic choice picks badly.
pub fn override_from_env() -> Option<Backend> {
    parse_override(std::env::var("BECKON_AUDIO").ok().as_deref())
}

pub fn parse_override(raw: Option<&str>) -> Option<Backend> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "null" | "none" | "off" | "silent" => Some(Backend::Null),
        "bell" => Some(Backend::Bell),
        "embedded" => Some(Backend::Embedded),
        // An unrecognized value must not silently mean "default"; treat it as a
        // named system player so a typo fails visibly in `doctor`.
        other => SYSTEM_PLAYERS
            .iter()
            .find(|(program, _)| *program == other)
            .map(|(program, _)| Backend::System(program)),
    }
}

/// The first system player present on `PATH`.
pub fn available_system_player() -> Option<&'static str> {
    SYSTEM_PLAYERS
        .iter()
        .map(|(program, _)| *program)
        .find(|p| on_path(p))
}

/// Every system player, with whether it is installed. For `beckon doctor`.
pub fn system_player_report() -> Vec<(&'static str, bool)> {
    SYSTEM_PLAYERS
        .iter()
        .map(|(program, _)| (*program, on_path(program)))
        .collect()
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file() || path.with_extension("exe").is_file()
}

/// Scale by user volume, clamped. Kept separate so it is testable without audio.
pub fn scaled(pcm: &Pcm, volume: f32) -> Vec<f32> {
    let volume = if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    };
    pcm.samples.iter().map(|s| s * volume).collect()
}

/// How long to wait after the mixer drains, before tearing the stream down.
///
/// `sleep_until_end` returns once the *mixer* has consumed the source, but the
/// device buffer still holds audio; dropping the stream at that moment clips the
/// tail. Measured at roughly 30ms here, so this is generous — and it costs
/// nothing, because playback always happens in a detached child.
#[cfg(feature = "embedded-audio")]
const DEVICE_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(feature = "embedded-audio")]
fn play_embedded(pcm: &Pcm, volume: f32) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = rodio::OutputStreamBuilder::open_default_stream()?;
    // rodio narrates its own teardown on stderr otherwise, which in a hook is
    // the user's agent session.
    stream.log_on_drop(false);

    let sink = rodio::Sink::connect_new(stream.mixer());
    sink.append(rodio::buffer::SamplesBuffer::new(
        pcm.channels,
        pcm.sample_rate,
        scaled(pcm, volume),
    ));

    // Blocking here is the point: this process must outlive the sound.
    sink.sleep_until_end();
    std::thread::sleep(DEVICE_SETTLE);
    Ok(())
}

fn play_system(pcm: &Pcm, volume: f32, program: &str) -> std::io::Result<()> {
    let scaled = Pcm {
        samples: scaled(pcm, volume),
        ..pcm.clone()
    };
    let path = create_temp_wav(&scaled)?;

    let args = SYSTEM_PLAYERS
        .iter()
        .find(|(name, _)| *name == program)
        .map(|(_, args)| *args)
        .unwrap_or(&[]);

    let result = spawn_player(program, args, &path);
    let _ = std::fs::remove_file(&path);
    result
}

/// Write the WAV to a fresh temp file that cannot be a pre-planted symlink.
///
/// `fs::write` follows symlinks and truncates, and a name built only from the
/// PID is guessable — the classic insecure-temp pattern. `create_new` refuses
/// to open anything that already exists, symlink included, so the only way
/// through is a name nobody has taken.
fn create_temp_wav(pcm: &Pcm) -> std::io::Result<PathBuf> {
    use std::io::Write;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    for attempt in 0..64u32 {
        let path = std::env::temp_dir().join(format!(
            "beckon-{}-{}-{attempt}.wav",
            std::process::id(),
            nonce
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&wav::encode(pcm))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free temporary filename"))
}

#[cfg(target_os = "windows")]
fn spawn_player(program: &str, args: &[&str], path: &Path) -> std::io::Result<()> {
    let script = format!(
        "(New-Object Media.SoundPlayer '{}').PlaySync()",
        path.display()
    );
    std::process::Command::new(program)
        .args(args)
        .arg(script)
        .status()
        .map(|_| ())
}

#[cfg(not(target_os = "windows"))]
fn spawn_player(program: &str, args: &[&str], path: &Path) -> std::io::Result<()> {
    std::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .map(|_| ())
}

/// Write a bell to the terminal, preferring the tty over stderr so it lands
/// even when stderr is captured.
fn ring_bell() {
    use std::io::Write;
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        if tty.write_all(b"\x07").is_ok() {
            return;
        }
    }
    let _ = std::io::stderr().write_all(b"\x07");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone() -> Pcm {
        Pcm {
            sample_rate: 48_000,
            channels: 2,
            samples: vec![0.5, -0.5, 0.25, -0.25],
        }
    }

    #[test]
    fn a_forced_null_backend_stays_silent_and_reports_itself() {
        assert_eq!(play_with(&tone(), 1.0, Some(Backend::Null)), Backend::Null);
    }

    #[test]
    fn the_env_override_recognizes_every_spelling_of_silence() {
        for raw in ["null", "none", "off", "silent", "NULL", " null "] {
            assert_eq!(parse_override(Some(raw)), Some(Backend::Null), "{raw:?}");
        }
    }

    #[test]
    fn the_env_override_recognizes_the_other_backends() {
        assert_eq!(parse_override(Some("bell")), Some(Backend::Bell));
        assert_eq!(parse_override(Some("embedded")), Some(Backend::Embedded));
    }

    #[test]
    fn an_unset_or_unrecognized_override_means_choose_automatically() {
        assert_eq!(parse_override(None), None);
        assert_eq!(parse_override(Some("gramophone")), None);
        assert_eq!(parse_override(Some("")), None);
    }

    #[test]
    fn a_named_system_player_can_be_forced() {
        // Whichever platform we are on, its first listed player is valid input.
        let first = SYSTEM_PLAYERS[0].0;
        assert_eq!(parse_override(Some(first)), Some(Backend::System(first)));
    }

    #[test]
    fn volume_scales_samples_and_is_clamped() {
        let pcm = tone();
        assert_eq!(scaled(&pcm, 1.0), pcm.samples);
        assert_eq!(scaled(&pcm, 0.0), vec![0.0; 4]);
        assert_eq!(scaled(&pcm, 0.5)[0], 0.25);
        // Out-of-range and non-finite volumes must not amplify or poison.
        assert_eq!(scaled(&pcm, 9.0), pcm.samples, "clamped to 1.0");
        assert_eq!(scaled(&pcm, -1.0), vec![0.0; 4], "clamped to 0.0");
        assert_eq!(scaled(&pcm, f32::NAN), vec![0.0; 4]);
    }

    #[test]
    fn backend_names_are_stable_and_readable() {
        assert_eq!(Backend::Embedded.as_str(), "embedded");
        assert_eq!(Backend::Null.as_str(), "null");
        assert_eq!(
            Backend::System("paplay").to_string(),
            "system player (paplay)"
        );
        assert_eq!(Backend::Bell.to_string(), "bell");
    }

    #[test]
    fn the_system_player_report_covers_every_candidate() {
        let report = system_player_report();
        assert_eq!(report.len(), SYSTEM_PLAYERS.len());
        // Detection must agree with itself.
        let detected = available_system_player();
        match detected {
            Some(program) => assert!(report.iter().any(|(p, ok)| *p == program && *ok)),
            None => assert!(report.iter().all(|(_, ok)| !ok)),
        }
    }

    #[test]
    fn path_lookup_rejects_directories_and_missing_files() {
        assert!(!on_path("this-program-does-not-exist-anywhere-1234"));
        assert!(!is_executable(Path::new("/")));
        assert!(!is_executable(Path::new("/nonexistent/thing")));
    }
}
