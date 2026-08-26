//! The packs that ship inside the binary.
//!
//! All three are pure synth recipes, which is why they can be embedded as text:
//! no audio files, no download, and no licensing surface to audit.

use crate::pack::manifest::Pack;

const AURORA: &str = include_str!("builtin/aurora.toml");
const CIPHER: &str = include_str!("builtin/cipher.toml");
const UNIT_7: &str = include_str!("builtin/unit-7.toml");

/// Built-in pack ids, in the order `beckon packs` lists them.
pub const IDS: [&str; 3] = ["aurora", "cipher", "unit-7"];

/// The raw manifest text, for `beckon packs show` and for scaffolding.
pub fn source(id: &str) -> Option<&'static str> {
    match id {
        "aurora" => Some(AURORA),
        "cipher" => Some(CIPHER),
        "unit-7" => Some(UNIT_7),
        _ => None,
    }
}

/// Parse a built-in pack. `None` for an unknown id.
///
/// A built-in that fails to parse is a build-time bug, not a runtime condition;
/// the test below guarantees all three parse.
pub fn get(id: &str) -> Option<Pack> {
    Pack::parse(source(id)?, None::<std::path::PathBuf>).ok()
}

pub fn all() -> Vec<Pack> {
    IDS.iter().filter_map(|id| get(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::synth::render;
    use crate::core::event::State;
    use crate::pack::manifest::SoundDef;
    use crate::pack::resolve::resolve;

    #[test]
    fn every_builtin_parses() {
        assert_eq!(all().len(), IDS.len());
    }

    #[test]
    fn every_builtin_id_matches_its_manifest() {
        for id in IDS {
            assert_eq!(get(id).unwrap().meta.id, id);
        }
    }

    #[test]
    fn an_unknown_id_is_none() {
        assert!(get("nope").is_none());
        assert!(source("nope").is_none());
    }

    #[test]
    fn every_builtin_defines_all_nine_states() {
        // Built-ins set the bar: a community pack only needs the core three,
        // but the shipped ones should never fall back.
        for pack in all() {
            for state in State::ALL {
                assert!(
                    pack.sounds.contains_key(&state),
                    "{} is missing {state}",
                    pack.meta.id
                );
            }
        }
    }

    #[test]
    fn every_builtin_is_cc0() {
        for pack in all() {
            assert_eq!(pack.meta.license, "CC0-1.0", "{} is not CC0", pack.meta.id);
        }
    }

    #[test]
    fn every_builtin_declares_full_metadata() {
        for pack in all() {
            let m = &pack.meta;
            assert!(!m.name.is_empty(), "{} has no name", m.id);
            assert!(!m.description.is_empty(), "{} has no description", m.id);
            assert!(!m.author.is_empty(), "{} has no author", m.id);
            assert!(!m.version.is_empty(), "{} has no version", m.id);
        }
    }

    #[test]
    fn every_builtin_sound_is_only_synth() {
        // Embedded packs cannot carry sample files.
        for pack in all() {
            for (state, def) in &pack.sounds {
                assert!(
                    matches!(def, SoundDef::Synth(_)),
                    "{} {state} is not a synth sound",
                    pack.meta.id
                );
            }
        }
    }

    #[test]
    fn every_builtin_sound_renders_audible_finite_audio() {
        for pack in all() {
            for state in State::ALL {
                let (_, def) = resolve(&pack, state).unwrap();
                let SoundDef::Synth(synth) = def else {
                    unreachable!()
                };
                let pcm = render(synth, 0.0);
                let id = &pack.meta.id;
                assert!(
                    pcm.samples.iter().all(|s| s.is_finite()),
                    "{id} {state} produced non-finite samples"
                );
                assert!(
                    pcm.samples.iter().any(|s| s.abs() > 0.1),
                    "{id} {state} is inaudibly quiet"
                );
            }
        }
    }

    #[test]
    fn every_builtin_sound_sits_in_a_usable_duration_band() {
        // Under ~80ms reads as a glitch; over 2s and you resent it.
        for pack in all() {
            for state in State::ALL {
                let (_, def) = resolve(&pack, state).unwrap();
                let SoundDef::Synth(synth) = def else {
                    unreachable!()
                };
                let ms = render(synth, 0.0).duration_ms();
                assert!(
                    (80.0..=2000.0).contains(&ms),
                    "{} {state} is {ms:.0}ms",
                    pack.meta.id
                );
            }
        }
    }

    #[test]
    fn every_builtin_sound_is_normalized_to_the_same_peak() {
        // Cross-pack loudness consistency: switching packs must not change how
        // loud beckon is.
        let target = 10f32.powf(-3.0 / 20.0);
        for pack in all() {
            for state in State::ALL {
                let (_, def) = resolve(&pack, state).unwrap();
                let SoundDef::Synth(synth) = def else {
                    unreachable!()
                };
                let pcm = render(synth, 0.0);
                let peak = pcm.samples.iter().fold(0f32, |m, v| m.max(v.abs()));
                assert!(
                    (peak - target).abs() < 0.02,
                    "{} {state} peaks at {peak}, want {target}",
                    pack.meta.id
                );
            }
        }
    }

    #[test]
    fn the_three_packs_actually_sound_different() {
        // Guards against a copy-paste pack that is only nominally distinct.
        let rendered: Vec<Vec<f32>> = all()
            .iter()
            .map(|p| {
                let (_, def) = resolve(p, State::Done).unwrap();
                let SoundDef::Synth(s) = def else {
                    unreachable!()
                };
                render(s, 0.0).samples
            })
            .collect();
        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                assert_ne!(
                    rendered[i], rendered[j],
                    "packs {i} and {j} render identically"
                );
            }
        }
    }

    #[test]
    fn needs_you_is_not_quieter_than_done() {
        // The alert must never be the sound you miss. Both are peak-normalized,
        // so compare total energy rather than peak.
        for pack in all() {
            let energy = |state: State| -> f32 {
                let (_, def) = resolve(&pack, state).unwrap();
                let SoundDef::Synth(s) = def else {
                    unreachable!()
                };
                let pcm = render(s, 0.0);
                pcm.samples.iter().map(|v| v * v).sum::<f32>() / pcm.frames().max(1) as f32
            };
            let (alert, done) = (energy(State::NeedsYou), energy(State::Done));
            assert!(
                alert >= done * 0.7,
                "{}: needs-you ({alert:.4}) is much weaker than done ({done:.4})",
                pack.meta.id
            );
        }
    }
}
