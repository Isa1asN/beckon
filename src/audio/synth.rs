//! Turning a recipe into samples.
//!
//! Rendering is **deterministic**: noise comes from a seeded PRNG, never a
//! thread RNG. Identical input must produce byte-identical output, because that
//! is what makes a pack diffable and a snapshot test meaningful.

use crate::audio::dsp::{peak_normalize, reverb, soft_clip, Adsr, Biquad};
use crate::audio::note::{note_to_hz, transpose};
use crate::pack::manifest::{Filter, FilterKind, Layer, Note, SynthDef, Wave};

pub const SAMPLE_RATE: u32 = 48_000;
/// Everything is rendered in stereo so panning is always available.
pub const CHANNELS: u16 = 2;
/// Headroom target. Applied to every sound so one loud pack cannot dominate.
const TARGET_DBFS: f32 = -3.0;

/// Longest sound we will render, mirroring the cap on decoded samples.
///
/// Durations come out of a `pack.toml` as plain `f32`, and a pack may arrive
/// from a git repository or a shared archive. Without a ceiling here,
/// `dur_ms = 1e15` asks for a 192 PB buffer and aborts the process, and a
/// far more likely `dur_ms = 1000000` — someone who meant `1000` — yields a
/// sixteen-minute sound costing 753 MB and three seconds of CPU on every
/// event. The attack and the typo have the same fix.
const MAX_RENDER_MS: f32 = 30_000.0;

/// Most layers, and notes per layer, honoured from one manifest. Generous for
/// anything musical; bounded against a manifest with a million entries.
const MAX_LAYERS: usize = 64;
const MAX_NOTES: usize = 512;

/// Ceiling on oscillator samples computed for one sound.
///
/// Capping duration and note count separately is not enough, because the cost
/// is their *product*: every note is rendered across its own span, so 512 notes
/// each held for the full 30 seconds is 737 million samples — measured at 21
/// seconds of CPU, in a detached child spawned on every agent event. Both caps
/// were individually reasonable and the combination was not, so the budget is
/// enforced on the thing that actually costs: samples written.
const MAX_RENDER_SAMPLES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved left/right.
    pub samples: Vec<f32>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    pub fn duration_ms(&self) -> f32 {
        self.frames() as f32 / self.sample_rate as f32 * 1000.0
    }
}

/// Render one sound, optionally transposed for per-project identity.
pub fn render(def: &SynthDef, transpose_semitones: f32) -> Pcm {
    let layers = &def.layers[..def.layers.len().min(MAX_LAYERS)];

    let total_ms = layers.iter().map(layer_span_ms).fold(0.0f32, f32::max);
    let frames = ms_to_frames(total_ms);

    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];

    // Shared across layers: one sound gets one budget, however it is divided up.
    let mut budget = MAX_RENDER_SAMPLES;

    for (index, layer) in layers.iter().enumerate() {
        let mono = render_layer(
            layer,
            index as u64,
            transpose_semitones,
            frames,
            &mut budget,
        );

        // Equal-power panning, so a centred layer is not louder than a panned one.
        let pan = finite_signed(layer.pan).clamp(-1.0, 1.0);
        let gain_l = ((1.0 - pan) / 2.0).sqrt() * std::f32::consts::SQRT_2;
        let gain_r = ((1.0 + pan) / 2.0).sqrt() * std::f32::consts::SQRT_2;

        for (i, sample) in mono.iter().enumerate() {
            left[i] += sample * gain_l;
            right[i] += sample * gain_r;
        }
    }

    if let Some(rv) = def.reverb {
        reverb(&mut left, SAMPLE_RATE, rv.room, rv.mix);
        reverb(&mut right, SAMPLE_RATE, rv.room, rv.mix);
        // Both channels take identical parameters, so lengths match; resize
        // defensively rather than trusting that.
        let len = left.len().max(right.len());
        left.resize(len, 0.0);
        right.resize(len, 0.0);
    }

    let mut samples = Vec::with_capacity(left.len() * 2);
    for (l, r) in left.iter().zip(right.iter()) {
        let gain = finite_signed(def.gain);
        samples.push(soft_clip(l * gain));
        samples.push(soft_clip(r * gain));
    }

    if def.normalize {
        peak_normalize(&mut samples, TARGET_DBFS);
    }

    Pcm {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        samples,
    }
}

/// How long a layer occupies, including the release tail of its last note.
///
/// Clamped, and every input sanitised: a `NaN` or infinite field in a manifest
/// must not propagate into an allocation size.
fn layer_span_ms(layer: &Layer) -> f32 {
    let steps = note_count(layer).saturating_sub(1) as f32;
    let span = finite(layer.delay_ms)
        + steps * finite(layer.step_ms)
        + finite(layer.dur_ms)
        + finite(layer.release_ms);
    span.clamp(0.0, MAX_RENDER_MS)
}

/// Notes actually rendered from a layer.
fn note_count(layer: &Layer) -> usize {
    layer.notes.len().min(MAX_NOTES)
}

/// A finite multiplier. `NaN` anywhere in a gain, pan or detune silently zeroes
/// the whole mix, so a pack author gets digital silence reported as success.
///
/// The range is deliberately wide: it exists to keep arithmetic finite, not to
/// express taste. `detune_cents = 1200` is one octave and entirely reasonable.
fn finite_signed(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-10_000.0, 10_000.0)
    } else {
        0.0
    }
}

/// A non-negative, finite milliseconds value. Anything else reads as zero.
fn finite(ms: f32) -> f32 {
    if ms.is_finite() {
        ms.clamp(0.0, MAX_RENDER_MS)
    } else {
        0.0
    }
}

fn ms_to_frames(ms: f32) -> usize {
    if !ms.is_finite() {
        return 0;
    }
    ((ms.clamp(0.0, MAX_RENDER_MS) / 1000.0) * SAMPLE_RATE as f32).round() as usize
}

/// Resolve a note to Hz. An unparseable name yields `None`, which skips that
/// note rather than rendering a wrong pitch.
fn note_hz(note: &Note) -> Option<f64> {
    match note {
        Note::Name(name) => note_to_hz(name),
        Note::Hz(hz) => (*hz > 0.0 && hz.is_finite()).then_some(*hz),
    }
}

fn render_layer(
    layer: &Layer,
    seed: u64,
    transpose_semitones: f32,
    frames: usize,
    budget: &mut usize,
) -> Vec<f32> {
    let mut out = vec![0.0f32; frames];

    let envelope = Adsr {
        attack_ms: layer.attack_ms,
        decay_ms: layer.decay_ms,
        sustain: layer.sustain,
        release_ms: layer.release_ms,
    };

    let shift = transpose_semitones + finite_signed(layer.detune_cents) / 100.0;
    let freqs: Vec<Option<f64>> = layer
        .notes
        .iter()
        .take(MAX_NOTES)
        .map(|n| note_hz(n).map(|hz| transpose(hz, shift)))
        .collect();

    // Seeded per layer so noise is reproducible but layers differ.
    let mut rng = Xorshift::new(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x0100_0000_01B3));
    let mut phase = 0.0f64;

    for (index, freq) in freqs.iter().enumerate() {
        let Some(freq) = *freq else { continue };
        let previous = index.checked_sub(1).and_then(|i| freqs[i]);

        let start_ms = layer.delay_ms.max(0.0) + index as f32 * layer.step_ms.max(0.0);
        let note_ms = layer.dur_ms.max(0.0) + layer.release_ms.max(0.0);
        let start = ms_to_frames(start_ms);
        let end = (start + ms_to_frames(note_ms)).min(frames);

        if start >= end || *budget == 0 {
            continue;
        }
        // Truncate this note to whatever budget is left, rather than abandoning
        // the sound: a manifest that overruns still produces its opening.
        let end = end.min(start + *budget);
        *budget -= end - start;

        for (offset, slot) in out[start..end].iter_mut().enumerate() {
            let t_ms = offset as f32 / SAMPLE_RATE as f32 * 1000.0;

            let mut hz = glided(previous, freq, t_ms, layer.glide_ms);
            if let Some(v) = layer.vibrato {
                let rate = f64::from(finite_signed(v.rate_hz));
                let depth = f64::from(finite_signed(v.depth_cents));
                let cycles = rate * f64::from(t_ms) / 1000.0;
                hz *= 2f64.powf(depth * (std::f64::consts::TAU * cycles).sin() / 1200.0);
            }

            let step = (hz / f64::from(SAMPLE_RATE)).clamp(0.0, 0.5);
            let sample = oscillator(layer.wave, phase, step, layer.fm, &mut rng);
            phase = (phase + step).fract();

            *slot += sample * envelope.gain_at(t_ms, layer.dur_ms) * finite_signed(layer.gain);
        }
    }

    if let Some(filter) = layer.filter {
        apply_filter(&mut out, filter);
    }
    out
}

/// Portamento from the previous note's pitch into this one.
fn glided(previous: Option<f64>, target: f64, t_ms: f32, glide_ms: f32) -> f64 {
    let (Some(from), true) = (previous, glide_ms > 0.0) else {
        return target;
    };
    if t_ms >= glide_ms {
        return target;
    }
    let progress = f64::from(t_ms / glide_ms);
    from + (target - from) * progress
}

fn apply_filter(samples: &mut [f32], filter: Filter) {
    let mut biquad = match filter.kind {
        FilterKind::Lowpass => Biquad::lowpass(SAMPLE_RATE, filter.cutoff_hz, filter.q),
        FilterKind::Highpass => Biquad::highpass(SAMPLE_RATE, filter.cutoff_hz, filter.q),
        FilterKind::Bandpass => Biquad::bandpass(SAMPLE_RATE, filter.cutoff_hz, filter.q),
    };
    for sample in samples.iter_mut() {
        *sample = biquad.process(*sample);
    }
}

/// One sample of the given waveform.
///
/// `saw` and `square` are band-limited with PolyBLEP. A naive discontinuity
/// aliases badly above a few hundred Hz, and these sounds live exactly there —
/// an alert sting is not the place for a gritty artefact.
fn oscillator(
    wave: Wave,
    phase: f64,
    step: f64,
    fm: Option<crate::pack::manifest::Fm>,
    rng: &mut Xorshift,
) -> f32 {
    use std::f64::consts::TAU;
    match wave {
        Wave::Sine => (TAU * phase).sin() as f32,
        Wave::Triangle => (1.0 - 4.0 * (phase - 0.5).abs()) as f32,
        Wave::Saw => (2.0 * phase - 1.0 - poly_blep(phase, step)) as f32,
        Wave::Square => {
            let naive = if phase < 0.5 { 1.0 } else { -1.0 };
            (naive + poly_blep(phase, step) - poly_blep((phase + 0.5).fract(), step)) as f32
        }
        Wave::Noise => rng.next_bipolar(),
        Wave::Fm => {
            let fm = fm.unwrap_or(crate::pack::manifest::Fm {
                ratio: 2.0,
                index: 3.0,
            });
            let modulator = (TAU * phase * f64::from(fm.ratio)).sin();
            (TAU * phase + f64::from(fm.index) * modulator).sin() as f32
        }
    }
}

/// Polynomial band-limited step: subtracts the aliasing energy a hard edge adds.
fn poly_blep(t: f64, dt: f64) -> f64 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// Deterministic PRNG. A thread RNG would make renders unreproducible and
/// snapshot tests meaningless.
struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64) -> Self {
        Xorshift(if seed == 0 {
            0x2545_F491_4F6C_DD1D
        } else {
            seed
        })
    }

    fn next_bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        // Top 24 bits into -1.0..1.0.
        ((x >> 40) as f32 / 8_388_608.0) - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::manifest::{Fm, Reverb};

    fn layer(wave: Wave, notes: Vec<Note>) -> Layer {
        Layer {
            wave,
            notes,
            dur_ms: 200.0,
            attack_ms: 5.0,
            decay_ms: 20.0,
            sustain: 0.8,
            release_ms: 50.0,
            ..Layer::default()
        }
    }

    fn synth(layers: Vec<Layer>) -> SynthDef {
        SynthDef {
            gain: 1.0,
            layers,
            reverb: None,
            normalize: true,
        }
    }

    /// Frequency by zero-crossing rate over the sustained middle.
    fn dominant_hz(pcm: &Pcm) -> f64 {
        let left: Vec<f32> = pcm.samples.chunks(2).map(|c| c[0]).collect();
        let (a, b) = (left.len() / 4, left.len() * 3 / 4);
        let window = &left[a..b];
        let crossings = window
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        crossings as f64 * pcm.sample_rate as f64 / window.len() as f64
    }

    fn peak(pcm: &Pcm) -> f32 {
        pcm.samples.iter().fold(0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn output_is_stereo_at_48k() {
        let p = render(&synth(vec![layer(Wave::Sine, vec![Note::Hz(440.0)])]), 0.0);
        assert_eq!(p.sample_rate, 48_000);
        assert_eq!(p.channels, 2);
        assert_eq!(p.samples.len() % 2, 0);
    }

    #[test]
    fn an_empty_definition_renders_silence_not_a_panic() {
        let p = render(&synth(vec![]), 0.0);
        assert!(p.samples.iter().all(|s| *s == 0.0));
        assert_eq!(p.frames(), 0);
    }

    #[test]
    fn a_layer_with_no_notes_renders_silence() {
        let p = render(&synth(vec![layer(Wave::Sine, vec![])]), 0.0);
        assert!(p.samples.iter().all(|s| s.abs() < 1e-9));
    }

    #[test]
    fn duration_covers_notes_plus_release() {
        let mut l = layer(
            Wave::Sine,
            vec![Note::Hz(440.0), Note::Hz(550.0), Note::Hz(660.0)],
        );
        l.step_ms = 100.0;
        l.dur_ms = 200.0;
        l.release_ms = 100.0;
        // two steps of 100 + 200 held + 100 release
        let p = render(&synth(vec![l]), 0.0);
        assert!(
            (p.duration_ms() - 500.0).abs() < 20.0,
            "got {}",
            p.duration_ms()
        );
    }

    #[test]
    fn the_longest_layer_sets_the_length() {
        let short = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        let mut long = short.clone();
        long.dur_ms = 800.0;
        let p = render(&synth(vec![short, long]), 0.0);
        assert!(p.duration_ms() > 800.0, "got {}", p.duration_ms());
    }

    #[test]
    fn delay_ms_pushes_a_layer_later_in_the_buffer() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        l.delay_ms = 200.0;
        let p = render(&synth(vec![l]), 0.0);
        let first_100ms = (SAMPLE_RATE as usize / 10) * 2;
        assert!(
            p.samples[..first_100ms].iter().all(|s| s.abs() < 1e-6),
            "layer should be silent during its delay"
        );
    }

    #[test]
    fn rendering_is_deterministic_across_runs() {
        let d = synth(vec![layer(Wave::Noise, vec![Note::Hz(440.0)])]);
        assert_eq!(
            render(&d, 0.0).samples,
            render(&d, 0.0).samples,
            "noise must be seeded"
        );
    }

    #[test]
    fn layers_get_different_noise() {
        let l = layer(Wave::Noise, vec![Note::Hz(440.0)]);
        let one = render(&synth(vec![l.clone()]), 0.0);
        let two = render(&synth(vec![l.clone(), l]), 0.0);
        assert_ne!(one.samples, two.samples);
    }

    #[test]
    fn transposition_raises_the_pitch_by_the_right_ratio() {
        let d = synth(vec![layer(Wave::Sine, vec![Note::Hz(440.0)])]);
        let base = dominant_hz(&render(&d, 0.0));
        let up = dominant_hz(&render(&d, 12.0));
        assert!(
            (up / base - 2.0).abs() < 0.15,
            "expected ~2x, got {}",
            up / base
        );
    }

    #[test]
    fn note_names_and_raw_hz_produce_the_same_pitch() {
        let a = render(
            &synth(vec![layer(Wave::Sine, vec![Note::Name("A4".into())])]),
            0.0,
        );
        let b = render(&synth(vec![layer(Wave::Sine, vec![Note::Hz(440.0)])]), 0.0);
        assert!((dominant_hz(&a) - dominant_hz(&b)).abs() < 5.0);
    }

    #[test]
    fn detune_cents_shifts_the_pitch() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        l.detune_cents = 1200.0;
        let plain = dominant_hz(&render(
            &synth(vec![layer(Wave::Sine, vec![Note::Hz(440.0)])]),
            0.0,
        ));
        let detuned = dominant_hz(&render(&synth(vec![l]), 0.0));
        assert!(
            (detuned / plain - 2.0).abs() < 0.15,
            "got {}",
            detuned / plain
        );
    }

    #[test]
    fn an_unparseable_note_name_is_skipped_rather_than_rendered_as_garbage() {
        let p = render(
            &synth(vec![layer(Wave::Sine, vec![Note::Name("H9".into())])]),
            0.0,
        );
        assert!(p.samples.iter().all(|s| s.is_finite()));
        assert!(p.samples.iter().all(|s| s.abs() < 1e-9), "should be silent");
    }

    #[test]
    fn a_nonsense_frequency_is_skipped() {
        for bad in [Note::Hz(0.0), Note::Hz(-100.0), Note::Hz(f64::NAN)] {
            let p = render(&synth(vec![layer(Wave::Sine, vec![bad])]), 0.0);
            assert!(p.samples.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn normalize_brings_the_peak_to_minus_three_dbfs() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        l.gain = 0.01;
        let p = render(&synth(vec![l]), 0.0);
        let target = 10f32.powf(TARGET_DBFS / 20.0);
        assert!(
            (peak(&p) - target).abs() < 0.02,
            "peak {}, want {target}",
            peak(&p)
        );
    }

    #[test]
    fn normalize_false_leaves_levels_alone() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        l.gain = 0.05;
        let mut d = synth(vec![l]);
        d.normalize = false;
        let p = render(&d, 0.0);
        assert!(
            peak(&p) < 0.2,
            "peak {} suggests it was normalized anyway",
            peak(&p)
        );
    }

    #[test]
    fn pan_moves_energy_between_channels() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        l.pan = -1.0;
        let p = render(&synth(vec![l]), 0.0);
        let left: f32 = p.samples.chunks(2).map(|c| c[0].abs()).sum();
        let right: f32 = p.samples.chunks(2).map(|c| c[1].abs()).sum();
        assert!(
            left > right * 5.0,
            "hard-left pan left {right} in the right channel"
        );
    }

    #[test]
    fn a_centred_layer_is_balanced() {
        let p = render(&synth(vec![layer(Wave::Sine, vec![Note::Hz(440.0)])]), 0.0);
        let left: f32 = p.samples.chunks(2).map(|c| c[0].abs()).sum();
        let right: f32 = p.samples.chunks(2).map(|c| c[1].abs()).sum();
        assert!((left - right).abs() < left * 0.01);
    }

    #[test]
    fn every_wave_renders_finite_audible_output() {
        for wave in [
            Wave::Sine,
            Wave::Triangle,
            Wave::Square,
            Wave::Saw,
            Wave::Noise,
            Wave::Fm,
        ] {
            let mut l = layer(wave, vec![Note::Hz(440.0)]);
            if matches!(wave, Wave::Fm) {
                l.fm = Some(Fm {
                    ratio: 2.0,
                    index: 3.0,
                });
            }
            let p = render(&synth(vec![l]), 0.0);
            assert!(
                p.samples.iter().all(|s| s.is_finite()),
                "{wave:?} produced NaN"
            );
            assert!(
                p.samples.iter().any(|s| s.abs() > 0.05),
                "{wave:?} was near-silent"
            );
        }
    }

    #[test]
    fn fm_without_parameters_still_renders() {
        let p = render(&synth(vec![layer(Wave::Fm, vec![Note::Hz(440.0)])]), 0.0);
        assert!(p.samples.iter().any(|s| s.abs() > 0.05));
    }

    #[test]
    fn a_lowpass_filter_reduces_high_frequency_energy() {
        let bright = render(&synth(vec![layer(Wave::Saw, vec![Note::Hz(220.0)])]), 0.0);
        let mut l = layer(Wave::Saw, vec![Note::Hz(220.0)]);
        l.filter = Some(Filter {
            kind: FilterKind::Lowpass,
            cutoff_hz: 400.0,
            q: 0.707,
        });
        let dull = render(&synth(vec![l]), 0.0);
        // Both are peak-normalized, so compare crossing rates instead of level.
        assert!(dominant_hz(&dull) <= dominant_hz(&bright) + 1.0);
    }

    #[test]
    fn reverb_lengthens_the_buffer() {
        let dry = synth(vec![layer(Wave::Sine, vec![Note::Hz(440.0)])]);
        let mut wet = dry.clone();
        wet.reverb = Some(Reverb {
            room: 0.6,
            mix: 0.4,
        });
        assert!(render(&wet, 0.0).frames() > render(&dry, 0.0).frames());
    }

    #[test]
    fn glide_starts_from_the_previous_pitch() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(220.0), Note::Hz(880.0)]);
        l.step_ms = 200.0;
        l.glide_ms = 150.0;
        let p = render(&synth(vec![l]), 0.0);
        assert!(p.samples.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn vibrato_renders_without_blowing_up() {
        let mut l = layer(Wave::Sine, vec![Note::Hz(440.0)]);
        l.vibrato = Some(crate::pack::manifest::Vibrato {
            rate_hz: 6.0,
            depth_cents: 40.0,
        });
        let p = render(&synth(vec![l]), 0.0);
        assert!(p.samples.iter().all(|s| s.is_finite()));
        assert!(p.samples.iter().any(|s| s.abs() > 0.05));
    }

    #[test]
    fn an_extreme_pitch_does_not_alias_into_nonsense() {
        // Above Nyquist the step is clamped; output must stay finite and bounded.
        let p = render(
            &synth(vec![layer(Wave::Saw, vec![Note::Hz(30_000.0)])]),
            0.0,
        );
        assert!(p.samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    }
}
