//! The pack manifest format.
//!
//! Every default lives in one place — the `Default` impls — and serde's
//! container-level `default` pulls from them, so the documented defaults and
//! the real ones cannot drift apart.

use crate::core::event::State;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

// --------------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("unknown sound key `{key}` — valid keys are: {valid}")]
    UnknownState { key: String, valid: String },
}

// ---------------------------------------------------------------------- types

#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    pub meta: PackMeta,
    pub sounds: BTreeMap<State, SoundDef>,
    /// Directory the manifest was loaded from. `None` for built-ins, which are
    /// embedded in the binary and therefore cannot reference sample files.
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    /// SPDX identifier. Required — a pack without provenance is not
    /// distributable, and the community library depends on this being checkable.
    pub license: String,
    #[serde(default)]
    pub description: String,
    /// Manifest format compatibility, e.g. `"^0"`.
    #[serde(default = "default_compat")]
    pub beckon: String,
}

fn default_compat() -> String {
    "^0".to_string()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum SoundDef {
    Synth(SynthDef),
    Sample(SampleDef),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SynthDef {
    pub gain: f32,
    /// Mixed together. TOML spells this `[[sounds.done.layer]]`.
    #[serde(rename = "layer")]
    pub layers: Vec<Layer>,
    pub reverb: Option<Reverb>,
    pub normalize: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleDef {
    pub file: String,
    /// Per-file provenance, because a pack may mix its own synth sounds with
    /// borrowed audio under a different licence.
    ///
    /// Required, with no default. A container-level `default` here would let a
    /// pack declare a sample with no licence at all and still parse — and the
    /// community library only works if provenance is checkable.
    pub license: String,
    #[serde(default = "one")]
    pub gain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wave {
    Sine,
    Triangle,
    Square,
    Saw,
    Noise,
    Fm,
}

/// A pitch. A TOML string is scientific notation; a number is raw Hz.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Note {
    Name(String),
    Hz(f64),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layer {
    pub wave: Wave,
    /// A sequence is an arpeggio; a single element is one tone.
    pub notes: Vec<Note>,
    /// Gap between successive notes.
    pub step_ms: f32,
    /// How long each note is held, before release.
    pub dur_ms: f32,
    /// Offset before this layer starts.
    pub delay_ms: f32,
    pub gain: f32,
    /// -1.0 hard left, 1.0 hard right.
    pub pan: f32,
    pub attack_ms: f32,
    pub decay_ms: f32,
    pub sustain: f32,
    pub release_ms: f32,
    /// Portamento between successive notes.
    pub glide_ms: f32,
    pub detune_cents: f32,
    pub fm: Option<Fm>,
    pub filter: Option<Filter>,
    pub vibrato: Option<Vibrato>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fm {
    /// Modulator frequency as a multiple of the carrier.
    pub ratio: f32,
    pub index: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub kind: FilterKind,
    pub cutoff_hz: f32,
    #[serde(default = "default_q")]
    pub q: f32,
}

fn default_q() -> f32 {
    0.707
}

fn one() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterKind {
    Lowpass,
    Highpass,
    Bandpass,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vibrato {
    pub rate_hz: f32,
    pub depth_cents: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reverb {
    pub room: f32,
    pub mix: f32,
}

// ------------------------------------------------------------------- defaults

impl Default for SynthDef {
    fn default() -> Self {
        SynthDef {
            gain: 1.0,
            layers: Vec::new(),
            reverb: None,
            normalize: true,
        }
    }
}

impl Default for SampleDef {
    fn default() -> Self {
        SampleDef {
            file: String::new(),
            license: String::new(),
            gain: 1.0,
        }
    }
}

impl Default for Layer {
    fn default() -> Self {
        Layer {
            wave: Wave::Sine,
            notes: Vec::new(),
            step_ms: 0.0,
            dur_ms: 250.0,
            delay_ms: 0.0,
            gain: 1.0,
            pan: 0.0,
            attack_ms: 5.0,
            decay_ms: 40.0,
            sustain: 0.7,
            release_ms: 120.0,
            glide_ms: 0.0,
            detune_cents: 0.0,
            fm: None,
            filter: None,
            vibrato: None,
        }
    }
}

// -------------------------------------------------------------------- parsing

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPack {
    pack: PackMeta,
    #[serde(default)]
    sounds: BTreeMap<String, SoundDef>,
}

impl Pack {
    pub fn parse(text: &str, root: Option<PathBuf>) -> Result<Pack, ParseError> {
        let raw: RawPack = toml::from_str(text)?;
        let mut sounds = BTreeMap::new();
        for (key, def) in raw.sounds {
            // Keys arrive as strings so an unknown one names itself in the
            // error, rather than producing serde's generic map-key complaint.
            let state = State::parse(&key).ok_or_else(|| ParseError::UnknownState {
                key: key.clone(),
                valid: State::ALL.map(|s| s.as_str()).join(", "),
            })?;
            sounds.insert(state, def);
        }
        Ok(Pack {
            meta: raw.pack,
            sounds,
            root,
        })
    }

    /// Core states this pack is missing. Empty means it is installable.
    pub fn missing_core_states(&self) -> Vec<State> {
        State::ALL
            .into_iter()
            .filter(|s| s.is_core() && !self.sounds.contains_key(s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[pack]
id = "demo"
name = "Demo"
version = "1.0.0"
author = "someone"
license = "CC0-1.0"
description = "a demo"
beckon = "^0"

[sounds.done]
type = "synth"
gain = 0.8

  [[sounds.done.layer]]
  wave = "triangle"
  notes = ["C5", "E5", 880.0, 440]
  step_ms = 90
  dur_ms = 260
    [sounds.done.layer.filter]
    kind = "lowpass"
    cutoff_hz = 3200
    q = 0.7

  [sounds.done.reverb]
  room = 0.35
  mix = 0.25

[sounds.needs-you]
type = "synth"
  [[sounds.needs-you.layer]]
  wave = "square"
  notes = ["A5"]

[sounds.failed]
type = "sample"
file = "failed.ogg"
license = "CC0-1.0"
"#;

    fn synth(pack: &Pack, state: State) -> &SynthDef {
        match &pack.sounds[&state] {
            SoundDef::Synth(s) => s,
            other => panic!("expected synth, got {other:?}"),
        }
    }

    #[test]
    fn a_complete_manifest_parses() {
        let p = Pack::parse(FULL, None).unwrap();
        assert_eq!(p.meta.id, "demo");
        assert_eq!(p.meta.name, "Demo");
        assert_eq!(p.meta.license, "CC0-1.0");
        assert_eq!(p.meta.beckon, "^0");
        assert_eq!(p.sounds.len(), 3);
        assert_eq!(p.root, None);
    }

    #[test]
    fn kebab_case_state_keys_map_to_states() {
        let p = Pack::parse(FULL, None).unwrap();
        for s in [State::Done, State::NeedsYou, State::Failed] {
            assert!(p.sounds.contains_key(&s), "{s} missing");
        }
    }

    #[test]
    fn notes_accept_names_floats_and_integers_in_one_sequence() {
        let p = Pack::parse(FULL, None).unwrap();
        let notes = &synth(&p, State::Done).layers[0].notes;
        assert_eq!(notes.len(), 4);
        assert!(matches!(&notes[0], Note::Name(n) if n == "C5"));
        assert!(matches!(&notes[1], Note::Name(n) if n == "E5"));
        assert!(matches!(notes[2], Note::Hz(h) if (h - 880.0).abs() < 1e-9));
        assert!(
            matches!(notes[3], Note::Hz(h) if (h - 440.0).abs() < 1e-9),
            "a bare TOML integer must read as Hz, got {:?}",
            notes[3]
        );
    }

    #[test]
    fn omitted_layer_fields_take_their_documented_defaults() {
        let p = Pack::parse(FULL, None).unwrap();
        let layer = &synth(&p, State::NeedsYou).layers[0];
        assert_eq!(layer.dur_ms, 250.0);
        assert_eq!(layer.step_ms, 0.0);
        assert_eq!(layer.delay_ms, 0.0);
        assert_eq!(layer.gain, 1.0);
        assert_eq!(layer.pan, 0.0);
        assert_eq!(layer.attack_ms, 5.0);
        assert_eq!(layer.decay_ms, 40.0);
        assert_eq!(layer.sustain, 0.7);
        assert_eq!(layer.release_ms, 120.0);
        assert_eq!(layer.glide_ms, 0.0);
        assert_eq!(layer.detune_cents, 0.0);
        assert!(layer.fm.is_none());
        assert!(layer.filter.is_none());
        assert!(layer.vibrato.is_none());
    }

    #[test]
    fn omitted_sound_fields_take_their_documented_defaults() {
        let p = Pack::parse(FULL, None).unwrap();
        let s = synth(&p, State::NeedsYou);
        assert_eq!(s.gain, 1.0);
        assert!(s.normalize, "normalize must default on");
        assert!(s.reverb.is_none());
    }

    #[test]
    fn explicit_values_override_defaults() {
        let p = Pack::parse(FULL, None).unwrap();
        let done = synth(&p, State::Done);
        assert_eq!(done.gain, 0.8);
        assert_eq!(done.layers[0].step_ms, 90.0);
        assert_eq!(done.layers[0].dur_ms, 260.0);
        assert_eq!(done.reverb.unwrap().room, 0.35);
        let f = done.layers[0].filter.unwrap();
        assert_eq!(f.kind, FilterKind::Lowpass);
        assert_eq!(f.cutoff_hz, 3200.0);
        assert_eq!(f.q, 0.7);
    }

    #[test]
    fn filter_q_has_a_default() {
        let toml = FULL.replace("    q = 0.7\n", "");
        let p = Pack::parse(&toml, None).unwrap();
        assert!((synth(&p, State::Done).layers[0].filter.unwrap().q - 0.707).abs() < 1e-6);
    }

    #[test]
    fn sample_sounds_carry_their_own_license() {
        let p = Pack::parse(FULL, None).unwrap();
        match &p.sounds[&State::Failed] {
            SoundDef::Sample(s) => {
                assert_eq!(s.file, "failed.ogg");
                assert_eq!(s.license, "CC0-1.0");
                assert_eq!(s.gain, 1.0);
            }
            other => panic!("expected sample, got {other:?}"),
        }
    }

    #[test]
    fn a_sample_without_a_licence_is_rejected() {
        // Provenance is what makes the community library redistributable.
        let bad = FULL.replace(
            "file = \"failed.ogg\"\nlicense = \"CC0-1.0\"",
            "file = \"failed.ogg\"",
        );
        let err = Pack::parse(&bad, None).unwrap_err().to_string();
        assert!(err.contains("license"), "{err}");
    }

    #[test]
    fn a_sample_without_a_file_is_rejected() {
        let bad = FULL.replace("file = \"failed.ogg\"\n", "");
        assert!(Pack::parse(&bad, None).is_err());
    }

    #[test]
    fn the_root_directory_is_remembered_for_resolving_samples() {
        let root = PathBuf::from("/packs/demo");
        let p = Pack::parse(FULL, Some(root.clone())).unwrap();
        assert_eq!(p.root, Some(root));
    }

    #[test]
    fn an_unknown_state_key_is_an_error_naming_the_key_and_the_valid_ones() {
        // Rename every mention, including the nested `[[...layer]]` table, or
        // TOML complains about a typeless orphan before we ever see the key.
        let bad = FULL.replace("sounds.needs-you", "sounds.needs-me");
        let err = Pack::parse(&bad, None).unwrap_err().to_string();
        assert!(err.contains("needs-me"), "should name the bad key: {err}");
        assert!(err.contains("needs-you"), "should list valid keys: {err}");
    }

    #[test]
    fn a_missing_required_meta_field_is_an_error() {
        for field in ["id", "name", "version", "author", "license"] {
            let bad = FULL
                .lines()
                .filter(|l| !l.trim_start().starts_with(&format!("{field} = ")))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                Pack::parse(&bad, None).is_err(),
                "missing {field} should fail"
            );
        }
    }

    #[test]
    fn description_and_beckon_are_optional() {
        let bad = FULL
            .lines()
            .filter(|l| !l.starts_with("description = ") && !l.starts_with("beckon = "))
            .collect::<Vec<_>>()
            .join("\n");
        let p = Pack::parse(&bad, None).unwrap();
        assert_eq!(p.meta.description, "");
        assert_eq!(p.meta.beckon, "^0");
    }

    #[test]
    fn an_unknown_wave_is_an_error() {
        let bad = FULL.replace("wave = \"triangle\"", "wave = \"bagpipe\"");
        assert!(Pack::parse(&bad, None).is_err());
    }

    #[test]
    fn an_unknown_sound_type_is_an_error() {
        let bad = FULL.replace("type = \"sample\"", "type = \"midi\"");
        assert!(Pack::parse(&bad, None).is_err());
    }

    #[test]
    fn a_typo_in_a_layer_field_is_an_error_rather_than_being_ignored() {
        // Silently ignoring `dur_msec` would leave an author baffled.
        let bad = FULL.replace("dur_ms = 260", "dur_msec = 260");
        let err = Pack::parse(&bad, None).unwrap_err().to_string();
        assert!(err.contains("dur_msec"), "{err}");
    }

    #[test]
    fn a_typo_in_a_meta_field_is_an_error() {
        let bad = FULL.replace("author = ", "auther = ");
        assert!(Pack::parse(&bad, None).is_err());
    }

    #[test]
    fn malformed_toml_is_an_error_not_a_panic() {
        assert!(Pack::parse("{{{ not toml", None).is_err());
        assert!(Pack::parse("", None).is_err());
        assert!(Pack::parse("[pack]", None).is_err());
    }

    #[test]
    fn a_pack_with_no_sounds_parses_but_reports_its_missing_core_states() {
        let only_meta = FULL.split("[sounds.done]").next().unwrap();
        let p = Pack::parse(only_meta, None).unwrap();
        assert!(p.sounds.is_empty());
        assert_eq!(
            p.missing_core_states(),
            vec![State::Done, State::NeedsYou, State::Failed]
        );
    }

    #[test]
    fn a_complete_pack_is_missing_nothing() {
        let p = Pack::parse(FULL, None).unwrap();
        assert!(p.missing_core_states().is_empty());
    }

    #[test]
    fn missing_core_states_ignores_optional_states() {
        let toml = FULL.replace("[sounds.failed]", "[sounds.compacting]");
        let p = Pack::parse(&toml, None).unwrap();
        assert_eq!(p.missing_core_states(), vec![State::Failed]);
    }
}
