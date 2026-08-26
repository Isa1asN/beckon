//! Loading someone's own audio files.
//!
//! # Threat model
//!
//! Two very different sources of paths reach this module, and they are trusted
//! differently.
//!
//! **The user's own config** may name any file it likes. It is their machine
//! and their file; refusing absolute paths there would defeat the point.
//!
//! **A pack manifest** may not. A pack can arrive from a git repository or a
//! shared archive, so `file = "../../../.ssh/id_rsa"` has to be impossible —
//! and so does reaching the same place through a symlink. Pack samples are
//! resolved and then *verified to still be inside the pack* after the
//! filesystem has had its say.
//!
//! Independently of provenance, decoding is bounded: a regular file only, a
//! size cap before reading, and a sample-count cap during decode. Together
//! those stop a character device that never ends, a FIFO that blocks forever,
//! and a small file that decodes into hours of audio.
//!
//! Note that `[sounds]` overrides are refused from *project* config — see
//! `core::config` — so a repository you clone cannot make your machine read
//! files at all.

// Only the decoder uses these; without that feature they are dead weight, and a
// build with `-D warnings` would fail on them.
#[cfg(feature = "embedded-audio")]
use crate::audio::dsp::peak_normalize;
use crate::audio::synth::Pcm;
use std::path::{Component, Path, PathBuf};

/// Largest sample file we will open.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Longest sound we will decode. Also bounds memory: a compressed file that
/// expands into hours would otherwise be a trivial denial of service.
pub const MAX_DURATION_SECS: f32 = 30.0;

/// Absolute ceiling on decoded samples, whatever the container claims.
///
/// The duration cap alone is derived from the decoder's *declared* sample rate
/// and channel count, both attacker-controlled: a file announcing 384 kHz and
/// eight channels would stretch the same 30 seconds into ~92M samples. This is
/// generous for real audio — 30 s of 48 kHz 8-channel — and firm against a lie.
#[cfg(feature = "embedded-audio")]
const MAX_SAMPLES: usize = 48_000 * 8 * MAX_DURATION_SECS as usize;

/// Container formats beckon will decode.
pub const ALLOWED_EXTENSIONS: [&str; 5] = ["wav", "ogg", "oga", "flac", "mp3"];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SampleError {
    #[error("{0}: not an audio file beckon reads (allowed: wav, ogg, flac, mp3)")]
    BadExtension(String),

    #[error("{0}: resolves outside its pack")]
    Escapes(String),

    #[error("{0}: cannot be read, or is not a regular file")]
    Unreadable(String),

    #[error("{0}: larger than 10 MiB")]
    TooLarge(String),

    #[error("{0}: longer than 30 seconds")]
    TooLong(String),

    #[error("{0}: not audio beckon can decode")]
    Undecodable(String),
}

/// Resolve a pack-relative sample path, refusing anything that leaves the pack.
pub fn resolve_in_pack(pack_root: &Path, file: &str) -> Result<PathBuf, SampleError> {
    let named = Path::new(file);
    let label = || file.to_string();

    // Cheap structural rejections first, so an obviously hostile manifest never
    // reaches the filesystem at all.
    if named.is_absolute() {
        return Err(SampleError::Escapes(label()));
    }
    for component in named.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            // `..`, `/`, and Windows prefixes like `C:` or `\\server\share`.
            _ => return Err(SampleError::Escapes(label())),
        }
    }
    check_extension(named, &label())?;

    // Then the authoritative check: whatever symlinks and `.` segments resolve
    // to must still sit inside the pack.
    let root = pack_root
        .canonicalize()
        .map_err(|_| SampleError::Unreadable(pack_root.display().to_string()))?;
    let resolved = pack_root
        .join(named)
        .canonicalize()
        .map_err(|_| SampleError::Unreadable(label()))?;

    if !resolved.starts_with(&root) {
        return Err(SampleError::Escapes(label()));
    }
    Ok(resolved)
}

fn check_extension(path: &Path, label: &str) -> Result<(), SampleError> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        Ok(())
    } else {
        Err(SampleError::BadExtension(label.to_string()))
    }
}

/// Decode an audio file into PCM, bounded in every direction.
pub fn load(path: &Path) -> Result<Pcm, SampleError> {
    let label = path.display().to_string();
    check_extension(path, &label)?;

    let metadata = std::fs::metadata(path).map_err(|_| SampleError::Unreadable(label.clone()))?;
    // Regular files only. A character device (`/dev/zero`) or a FIFO would
    // otherwise stall the decode forever, and neither reports a useful size.
    if !metadata.is_file() {
        return Err(SampleError::Unreadable(label.clone()));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(SampleError::TooLarge(label));
    }

    decode(path, &label)
}

#[cfg(feature = "embedded-audio")]
fn decode(path: &Path, label: &str) -> Result<Pcm, SampleError> {
    use rodio::Source;

    let file = std::fs::File::open(path).map_err(|_| SampleError::Unreadable(label.to_string()))?;
    let decoder = rodio::Decoder::new(std::io::BufReader::new(file))
        .map_err(|_| SampleError::Undecodable(label.to_string()))?;

    let channels = decoder.channels().max(1);
    let sample_rate = decoder.sample_rate().max(1);

    // Cap by sample count rather than by trusting a declared duration, which a
    // hostile container can lie about — then cap that cap.
    let limit = ((MAX_DURATION_SECS * sample_rate as f32) as usize)
        .saturating_mul(channels as usize)
        .min(MAX_SAMPLES);

    let mut samples: Vec<f32> = Vec::new();
    for sample in decoder {
        if samples.len() >= limit {
            return Err(SampleError::TooLong(label.to_string()));
        }
        samples.push(sample);
    }

    if samples.is_empty() {
        return Err(SampleError::Undecodable(label.to_string()));
    }
    if samples.iter().any(|s| !s.is_finite()) {
        return Err(SampleError::Undecodable(label.to_string()));
    }

    // Same treatment synth output gets, so switching between a synth pack and
    // your own files does not change how loud beckon is.
    peak_normalize(&mut samples, -3.0);
    Ok(Pcm {
        sample_rate,
        channels,
        samples,
    })
}

#[cfg(not(feature = "embedded-audio"))]
fn decode(_path: &Path, label: &str) -> Result<Pcm, SampleError> {
    Err(SampleError::Undecodable(format!(
        "{label} (built without the embedded-audio feature)"
    )))
}

/// Playback rate for a sample under per-project identity.
///
/// Clamped hard: shifting a recorded sound more than a few semitones by
/// resampling makes it sound broken rather than distinct.
pub fn shifted_rate(sample_rate: u32, semitones: f32) -> u32 {
    const MAX_SHIFT: f32 = 3.0;
    let shift = semitones.clamp(-MAX_SHIFT, MAX_SHIFT);
    let factor = 2f32.powf(shift / 12.0);
    ((sample_rate as f32 * factor) as u32).clamp(4_000, 192_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::wav;

    /// A pack directory with a real, decodable sample inside it.
    fn pack_with_sample() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.wav");
        wav::write(&path, &tone(0.2)).unwrap();
        (dir, path)
    }

    fn tone(seconds: f32) -> Pcm {
        let rate = 8_000u32;
        let frames = (rate as f32 * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = (i as f32 * 0.05).sin() * 0.5;
            samples.push(v);
            samples.push(v);
        }
        Pcm {
            sample_rate: rate,
            channels: 2,
            samples,
        }
    }

    // ── path containment ────────────────────────────────────────────────

    #[test]
    fn a_pack_cannot_escape_with_dot_dot() {
        let (dir, _) = pack_with_sample();
        for hostile in [
            "../../../etc/passwd",
            "../outside.wav",
            "a/../../outside.wav",
            "./../outside.wav",
            "sub/../../../../../../etc/shadow",
        ] {
            assert_eq!(
                resolve_in_pack(dir.path(), hostile),
                Err(SampleError::Escapes(hostile.to_string())),
                "{hostile:?} was not refused"
            );
        }
    }

    #[test]
    fn a_pack_cannot_name_an_absolute_path() {
        let (dir, _) = pack_with_sample();
        for hostile in ["/etc/passwd", "/tmp/anything.wav", "//etc/passwd"] {
            assert!(
                matches!(
                    resolve_in_pack(dir.path(), hostile),
                    Err(SampleError::Escapes(_))
                ),
                "{hostile:?} was not refused"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_pack_cannot_escape_through_a_symlink() {
        // The check that actually matters: the structural rules above pass
        // cleanly here, and only canonicalization catches it.
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.wav");
        wav::write(&secret, &tone(0.1)).unwrap();

        let (dir, _) = pack_with_sample();
        std::os::unix::fs::symlink(&secret, dir.path().join("link.wav")).unwrap();

        assert_eq!(
            resolve_in_pack(dir.path(), "link.wav"),
            Err(SampleError::Escapes("link.wav".to_string()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_stays_inside_the_pack_is_fine() {
        let (dir, target) = pack_with_sample();
        std::os::unix::fs::symlink(&target, dir.path().join("alias.wav")).unwrap();
        assert!(resolve_in_pack(dir.path(), "alias.wav").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_parent_directory_cannot_be_used_to_escape() {
        let outside = tempfile::tempdir().unwrap();
        wav::write(&outside.path().join("secret.wav"), &tone(0.1)).unwrap();

        let (dir, _) = pack_with_sample();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("sub")).unwrap();

        assert_eq!(
            resolve_in_pack(dir.path(), "sub/secret.wav"),
            Err(SampleError::Escapes("sub/secret.wav".to_string()))
        );
    }

    #[test]
    fn ordinary_relative_paths_resolve() {
        let (dir, _) = pack_with_sample();
        assert!(resolve_in_pack(dir.path(), "ok.wav").is_ok());
        assert!(resolve_in_pack(dir.path(), "./ok.wav").is_ok());

        std::fs::create_dir(dir.path().join("sub")).unwrap();
        wav::write(&dir.path().join("sub/deep.wav"), &tone(0.1)).unwrap();
        assert!(resolve_in_pack(dir.path(), "sub/deep.wav").is_ok());
    }

    #[test]
    fn only_known_audio_extensions_are_accepted() {
        let (dir, _) = pack_with_sample();
        for hostile in [
            "evil.sh",
            "lib.so",
            "notes.txt",
            "noextension",
            "ok.wav.sh",
            "",
        ] {
            assert!(
                matches!(
                    resolve_in_pack(dir.path(), hostile),
                    Err(SampleError::BadExtension(_))
                ),
                "{hostile:?} was accepted"
            );
        }
    }

    #[test]
    fn extension_matching_ignores_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("LOUD.WAV");
        wav::write(&path, &tone(0.1)).unwrap();
        assert!(resolve_in_pack(dir.path(), "LOUD.WAV").is_ok());
    }

    #[test]
    fn a_missing_file_is_unreadable_rather_than_a_panic() {
        let (dir, _) = pack_with_sample();
        assert!(matches!(
            resolve_in_pack(dir.path(), "absent.wav"),
            Err(SampleError::Unreadable(_))
        ));
    }

    // ── bounded decoding ────────────────────────────────────────────────

    // Decoding only exists with the feature; without it these have no meaning.
    #[cfg(feature = "embedded-audio")]
    #[test]
    fn a_real_sample_decodes_and_is_normalized() {
        let (_dir, path) = pack_with_sample();
        let pcm = load(&path).unwrap();
        assert_eq!(pcm.channels, 2);
        assert_eq!(pcm.sample_rate, 8_000);
        let peak = pcm.samples.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 10f32.powf(-3.0 / 20.0)).abs() < 0.02, "peak {peak}");
    }

    #[cfg(unix)]
    #[test]
    fn a_character_device_is_refused_instead_of_read_forever() {
        // /dev/zero reports zero length and never ends. The size cap cannot
        // save us here; refusing non-regular files is what does.
        let result = load(Path::new("/dev/zero"));
        assert!(
            matches!(result, Err(SampleError::BadExtension(_))),
            "{result:?}"
        );

        // Even named so the extension check passes, via a symlink.
        let dir = tempfile::tempdir().unwrap();
        let disguised = dir.path().join("zero.wav");
        std::os::unix::fs::symlink("/dev/zero", &disguised).unwrap();
        assert_eq!(
            load(&disguised),
            Err(SampleError::Unreadable(disguised.display().to_string()))
        );
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let masquerade = dir.path().join("sounds.wav");
        std::fs::create_dir(&masquerade).unwrap();
        assert!(matches!(load(&masquerade), Err(SampleError::Unreadable(_))));
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.wav");
        // Sparse: costs nothing on disk, still reports its length.
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(file);
        assert!(matches!(load(&big), Err(SampleError::TooLarge(_))));
    }

    #[test]
    fn a_file_that_is_not_audio_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("text.wav", &b"this is not audio at all"[..]),
            ("empty.wav", &b""[..]),
            ("truncated.wav", &b"RIFF"[..]),
            ("nul.wav", &[0u8; 64][..]),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(
                matches!(load(&path), Err(SampleError::Undecodable(_))),
                "{name} did not fail cleanly: {:?}",
                load(&path)
            );
        }
    }

    // Decoding only exists with the feature; without it these have no meaning.
    #[cfg(feature = "embedded-audio")]
    #[test]
    fn a_sample_longer_than_the_cap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        // 35s at 8kHz mono stays well under the byte cap but over the time cap,
        // which is the shape a decompression bomb takes.
        let rate = 8_000u32;
        let frames = (rate as f32 * (MAX_DURATION_SECS + 5.0)) as usize;
        let pcm = Pcm {
            sample_rate: rate,
            channels: 1,
            samples: vec![0.1; frames],
        };
        wav::write(&path, &pcm).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() < MAX_FILE_BYTES);
        assert!(
            matches!(load(&path), Err(SampleError::TooLong(_))),
            "{:?}",
            load(&path)
        );
    }

    // Decoding only exists with the feature; without it these have no meaning.
    #[cfg(feature = "embedded-audio")]
    #[test]
    fn a_sample_just_under_the_cap_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.wav");
        let rate = 8_000u32;
        let frames = (rate as f32 * (MAX_DURATION_SECS - 5.0)) as usize;
        wav::write(
            &path,
            &Pcm {
                sample_rate: rate,
                channels: 1,
                samples: vec![0.1; frames],
            },
        )
        .unwrap();
        assert!(load(&path).is_ok());
    }

    #[cfg(not(feature = "embedded-audio"))]
    #[test]
    fn without_a_decoder_samples_fail_cleanly_rather_than_silently() {
        // A musl build ships without the decoder. Pointing it at a real file
        // must produce a reported error, not a panic and not silence that looks
        // like success.
        let (_dir, path) = pack_with_sample();
        assert!(matches!(load(&path), Err(SampleError::Undecodable(_))));
    }

    // ── identity shifting ───────────────────────────────────────────────

    #[test]
    fn identity_shifts_the_playback_rate() {
        assert_eq!(shifted_rate(48_000, 0.0), 48_000);
        assert!(shifted_rate(48_000, 2.0) > 48_000);
        assert!(shifted_rate(48_000, -2.0) < 48_000);
    }

    #[test]
    fn identity_shifting_is_clamped_so_samples_do_not_chipmunk() {
        let up = shifted_rate(48_000, 12.0);
        let capped = shifted_rate(48_000, 3.0);
        assert_eq!(up, capped, "a twelve-semitone shift must clamp to three");
        assert_eq!(shifted_rate(48_000, -12.0), shifted_rate(48_000, -3.0));
    }

    #[test]
    fn identity_shifting_survives_absurd_input() {
        for semitones in [f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30, -1e30] {
            let rate = shifted_rate(48_000, semitones);
            assert!((4_000..=192_000).contains(&rate), "{semitones} -> {rate}");
        }
    }

    #[test]
    fn identity_shifting_cannot_produce_a_zero_rate() {
        assert!(shifted_rate(1, -3.0) >= 4_000);
        assert!(shifted_rate(u32::MAX, 3.0) <= 192_000);
    }
}
