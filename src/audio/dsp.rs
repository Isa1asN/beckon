//! The small set of DSP primitives packs are built from.
//!
//! Deliberately minimal: enough to compose a calm starship, a stealth sting and
//! a lab robot, few enough that a pack author can predict what a number does.

/// Attack / decay / sustain / release, in milliseconds and a 0..1 sustain level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adsr {
    pub attack_ms: f32,
    pub decay_ms: f32,
    pub sustain: f32,
    pub release_ms: f32,
}

impl Adsr {
    /// Envelope gain at `t_ms` for a note held `dur_ms`.
    ///
    /// Release begins at `dur_ms`, so total audible length is
    /// `dur_ms + release_ms`. Always within `0.0..=1.0`.
    ///
    /// Release starts from whatever level the envelope had actually reached,
    /// **not** from `sustain`. For a note shorter than `attack + decay` the two
    /// differ, and jumping between them is an audible click on every short note
    /// — which is most of a percussive pack.
    pub fn gain_at(&self, t_ms: f32, dur_ms: f32) -> f32 {
        if t_ms < 0.0 {
            return 0.0;
        }
        let dur = dur_ms.max(0.0);

        let gain = if t_ms < dur {
            self.held_gain(t_ms)
        } else {
            let release = self.release_ms.max(0.0);
            if release <= 0.0 {
                0.0
            } else {
                let progress = (t_ms - dur) / release;
                if progress >= 1.0 {
                    0.0
                } else {
                    // Continuous at the boundary by construction.
                    self.held_gain(dur) * (1.0 - progress)
                }
            }
        };

        gain.clamp(0.0, 1.0)
    }

    /// Attack into decay into sustain, ignoring release entirely.
    fn held_gain(&self, t_ms: f32) -> f32 {
        let attack = self.attack_ms.max(0.0);
        let decay = self.decay_ms.max(0.0);
        let sustain = self.sustain.clamp(0.0, 1.0);

        if t_ms < attack {
            t_ms / attack
        } else if t_ms < attack + decay {
            let progress = (t_ms - attack) / decay;
            1.0 + (sustain - 1.0) * progress
        } else {
            sustain
        }
    }
}

/// A second-order IIR filter (RBJ cookbook coefficients).
#[derive(Debug, Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn lowpass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Biquad {
        let (sin, cos, alpha) = shape(sample_rate, cutoff_hz, q);
        let b0 = (1.0 - cos) / 2.0;
        Biquad::normalized(b0, 1.0 - cos, b0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            .unwrap_or_else(|| Biquad::passthrough(sin))
    }

    pub fn highpass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Biquad {
        let (sin, cos, alpha) = shape(sample_rate, cutoff_hz, q);
        let b0 = (1.0 + cos) / 2.0;
        Biquad::normalized(b0, -(1.0 + cos), b0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            .unwrap_or_else(|| Biquad::passthrough(sin))
    }

    /// Constant 0 dB peak gain, so raising Q narrows without getting louder.
    pub fn bandpass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Biquad {
        let (sin, cos, alpha) = shape(sample_rate, cutoff_hz, q);
        Biquad::normalized(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            .unwrap_or_else(|| Biquad::passthrough(sin))
    }

    fn normalized(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Option<Biquad> {
        if a0.abs() < f32::EPSILON || !a0.is_finite() {
            return None;
        }
        let q = Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        };
        let finite = [q.b0, q.b1, q.b2, q.a1, q.a2].iter().all(|c| c.is_finite());
        finite.then_some(q)
    }

    /// Degenerate coefficients would blow up; pass audio through untouched
    /// instead. `_seed` exists only to keep the signature uniform.
    fn passthrough(_seed: f32) -> Biquad {
        Biquad {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        if !y.is_finite() {
            // Reset rather than propagate. A blown filter must not poison a pack.
            *self = Biquad::passthrough(0.0);
            return 0.0;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A Schroeder reverb: parallel combs into series allpasses.
///
/// Extends `samples` by the tail length. `room` sets decay, `mix` the wet/dry
/// blend; `mix <= 0.0` is a no-op.
pub fn reverb(samples: &mut Vec<f32>, sample_rate: u32, room: f32, mix: f32) {
    let mix = mix.clamp(0.0, 1.0);
    if mix <= 0.0 || samples.is_empty() {
        return;
    }
    let room = room.clamp(0.0, 1.0);

    // Classic Schroeder delays, mutually non-harmonic so the tail does not ring.
    const COMB_MS: [f32; 4] = [29.7, 37.1, 41.1, 43.7];
    const ALLPASS_MS: [f32; 2] = [5.0, 1.7];
    const ALLPASS_G: f32 = 0.7;
    /// Peak steady-state wet gain. Above ~1.5 a sustained loud sound drives
    /// `soft_clip` into obvious distortion.
    const TARGET_WET_GAIN: f32 = 1.5;

    // Capped well below 1.0: a runaway reverb is a wall of noise in someone's
    // terminal, which is a far worse failure than a short tail.
    let feedback = 0.5 + 0.3 * room;
    let tail_ms = 250.0 + 1000.0 * room;

    // Each comb settles at 1/(1-feedback) for sustained input, so scaling by
    // (1-feedback) keeps total wet gain constant as the room grows. Without
    // this, a large room is not just longer but several times louder.
    let comb_scale = TARGET_WET_GAIN * (1.0 - feedback) / COMB_MS.len() as f32;

    let dry = samples.clone();
    let tail_len = ((tail_ms / 1000.0) * sample_rate as f32) as usize;
    samples.resize(dry.len() + tail_len, 0.0);
    let n = samples.len();

    let at = |i: usize| if i < dry.len() { dry[i] } else { 0.0 };

    let mut wet = vec![0.0f32; n];
    for delay_ms in COMB_MS {
        let delay = ((delay_ms / 1000.0) * sample_rate as f32) as usize;
        if delay == 0 || delay >= n {
            continue;
        }
        let mut line = vec![0.0f32; delay];
        let mut cursor = 0usize;
        for (i, out) in wet.iter_mut().enumerate() {
            let delayed = line[cursor];
            line[cursor] = at(i) + feedback * delayed;
            cursor = (cursor + 1) % delay;
            *out += delayed * comb_scale;
        }
    }

    for delay_ms in ALLPASS_MS {
        let delay = ((delay_ms / 1000.0) * sample_rate as f32) as usize;
        if delay == 0 || delay >= n {
            continue;
        }
        let mut line = vec![0.0f32; delay];
        let mut cursor = 0usize;
        for sample in wet.iter_mut() {
            let input = *sample;
            let delayed = line[cursor];
            let output = delayed - ALLPASS_G * input;
            line[cursor] = input + ALLPASS_G * output;
            cursor = (cursor + 1) % delay;
            *sample = output;
        }
    }

    for i in 0..n {
        samples[i] = at(i) * (1.0 - mix) + wet[i] * mix;
    }
}

/// Shared RBJ intermediate terms. Clamps keep degenerate inputs survivable.
fn shape(sample_rate: u32, cutoff_hz: f32, q: f32) -> (f32, f32, f32) {
    let nyquist_limit = sample_rate as f32 * 0.45;
    let cutoff = cutoff_hz.clamp(1.0, nyquist_limit.max(1.0));
    let q = q.clamp(0.05, 40.0);
    let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate as f32;
    let (sin, cos) = (w0.sin(), w0.cos());
    (sin, cos, sin / (2.0 * q))
}

/// Scale so the loudest sample sits at `target_dbfs`.
///
/// Every pack gets this treatment before user volume is applied. Without it,
/// one loud community pack ruins the experience and users blame beckon.
pub fn peak_normalize(samples: &mut [f32], target_dbfs: f32) {
    let peak = samples.iter().fold(0f32, |max, v| max.max(v.abs()));
    if peak <= f32::EPSILON || !peak.is_finite() {
        return;
    }
    let gain = 10f32.powf(target_dbfs / 20.0) / peak;
    for sample in samples.iter_mut() {
        *sample *= gain;
    }
}

/// Bounded, monotonic saturation. Cheaper than tracking headroom everywhere.
pub fn soft_clip(x: f32) -> f32 {
    x.tanh()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Adsr {
        Adsr {
            attack_ms: 10.0,
            decay_ms: 20.0,
            sustain: 0.5,
            release_ms: 50.0,
        }
    }

    /// Energy of a sine at `freq` after passing through `make(...)`.
    fn filtered_energy(mut filter: Biquad, sample_rate: u32, freq: f64) -> f32 {
        let mut sum = 0.0f32;
        let n = sample_rate as usize / 10;
        for i in 0..n {
            let x = (2.0 * std::f64::consts::PI * freq * i as f64 / sample_rate as f64).sin();
            let y = filter.process(x as f32);
            // Skip the transient so we measure steady state.
            if i > n / 2 {
                sum += y * y;
            }
        }
        sum
    }

    #[test]
    fn envelope_starts_silent_and_peaks_at_the_end_of_attack() {
        let e = env();
        assert!(e.gain_at(0.0, 200.0) < 0.01);
        assert!((e.gain_at(10.0, 200.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn envelope_decays_to_sustain_and_holds() {
        let e = env();
        assert!((e.gain_at(30.0, 200.0) - 0.5).abs() < 0.05);
        assert!((e.gain_at(100.0, 200.0) - 0.5).abs() < 0.05);
        assert!((e.gain_at(199.0, 200.0) - 0.5).abs() < 0.05);
    }

    #[test]
    fn envelope_is_continuous_where_release_begins() {
        // A jump here is an audible click on every note. It bit us once: the
        // release branch used to start from `sustain` regardless of the level
        // the envelope had actually reached.
        let e = env();
        for dur in [1.0, 5.0, 10.0, 20.0, 30.0, 44.0, 45.0, 60.0, 200.0] {
            let before = e.gain_at(dur - 0.01, dur);
            let after = e.gain_at(dur, dur);
            assert!(
                (before - after).abs() < 0.01,
                "discontinuity of {:.3} at dur={dur} ({before:.3} -> {after:.3})",
                (before - after).abs()
            );
        }
    }

    #[test]
    fn a_note_shorter_than_attack_releases_from_its_partial_attack_level() {
        let e = Adsr {
            attack_ms: 100.0,
            decay_ms: 50.0,
            sustain: 0.2,
            release_ms: 50.0,
        };
        // Held only 25ms, so the attack only reached 0.25.
        let at_release = e.gain_at(25.0, 25.0);
        assert!((at_release - 0.25).abs() < 0.02, "got {at_release}");
        // And it must not jump up to a higher level on the way down.
        assert!(e.gain_at(30.0, 25.0) < at_release);
    }

    #[test]
    fn envelope_is_monotonically_non_increasing_during_release() {
        let e = env();
        let mut previous = f32::MAX;
        let mut t = 200.0;
        while t < 260.0 {
            let g = e.gain_at(t, 200.0);
            assert!(g <= previous + 1e-6, "release rose at t={t}");
            previous = g;
            t += 0.5;
        }
    }

    #[test]
    fn envelope_releases_to_silence_by_the_end() {
        let e = env();
        assert!(e.gain_at(250.0, 200.0) < 0.01);
        assert!(e.gain_at(1000.0, 200.0) < 0.01);
    }

    #[test]
    fn envelope_stays_within_unit_range_everywhere() {
        let e = env();
        for i in 0..1000 {
            let g = e.gain_at(i as f32, 200.0);
            assert!((0.0..=1.0).contains(&g), "gain {g} out of range at t={i}");
        }
    }

    #[test]
    fn a_zero_length_envelope_does_not_divide_by_zero() {
        let e = Adsr {
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: 0.0,
        };
        for i in 0..300 {
            assert!(
                e.gain_at(i as f32, 200.0).is_finite(),
                "not finite at t={i}"
            );
        }
    }

    #[test]
    fn a_negative_time_reads_as_silence() {
        assert_eq!(env().gain_at(-5.0, 200.0), 0.0);
    }

    #[test]
    fn lowpass_attenuates_content_above_its_cutoff() {
        let sr = 48_000;
        let high = filtered_energy(Biquad::lowpass(sr, 1000.0, 0.707), sr, 8000.0);
        let low = filtered_energy(Biquad::lowpass(sr, 1000.0, 0.707), sr, 200.0);
        assert!(
            high < low * 0.1,
            "8kHz ({high}) should be far below 200Hz ({low})"
        );
    }

    #[test]
    fn highpass_does_the_opposite() {
        let sr = 48_000;
        let high = filtered_energy(Biquad::highpass(sr, 1000.0, 0.707), sr, 8000.0);
        let low = filtered_energy(Biquad::highpass(sr, 1000.0, 0.707), sr, 200.0);
        assert!(
            low < high * 0.1,
            "200Hz ({low}) should be far below 8kHz ({high})"
        );
    }

    #[test]
    fn bandpass_favours_its_centre() {
        let sr = 48_000;
        let centre = filtered_energy(Biquad::bandpass(sr, 1000.0, 2.0), sr, 1000.0);
        let below = filtered_energy(Biquad::bandpass(sr, 1000.0, 2.0), sr, 100.0);
        let above = filtered_energy(Biquad::bandpass(sr, 1000.0, 2.0), sr, 10000.0);
        assert!(below < centre * 0.5, "below={below} centre={centre}");
        assert!(above < centre * 0.5, "above={above} centre={centre}");
    }

    #[test]
    fn filters_never_produce_nan_even_on_pathological_input() {
        for mut f in [
            Biquad::lowpass(48_000, 1000.0, 0.707),
            Biquad::lowpass(48_000, 0.0, 0.0),
            Biquad::highpass(48_000, 1e9, 1e9),
        ] {
            for n in 0..10_000 {
                let y = f.process(if n % 2 == 0 { 1.0 } else { -1.0 });
                assert!(y.is_finite(), "filter blew up at n={n}");
            }
        }
    }

    #[test]
    fn peak_normalize_hits_the_target() {
        let mut s = vec![0.1, -0.05, 0.02];
        peak_normalize(&mut s, -3.0);
        let peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
        let target = 10f32.powf(-3.0 / 20.0);
        assert!((peak - target).abs() < 1e-4, "peak {peak} != {target}");
    }

    #[test]
    fn peak_normalize_attenuates_as_well_as_amplifies() {
        let mut s = vec![4.0, -2.0];
        peak_normalize(&mut s, -3.0);
        assert!(s[0] < 1.0);
    }

    #[test]
    fn peak_normalize_leaves_silence_alone() {
        let mut s = vec![0.0; 100];
        peak_normalize(&mut s, -3.0);
        assert!(s.iter().all(|v| *v == 0.0), "must not divide by zero");
    }

    #[test]
    fn peak_normalize_on_an_empty_slice_is_a_no_op() {
        let mut s: Vec<f32> = vec![];
        peak_normalize(&mut s, -3.0);
        assert!(s.is_empty());
    }

    #[test]
    fn reverb_extends_the_tail_without_exploding() {
        let sr = 48_000;
        let mut s = vec![0.0f32; sr as usize / 2];
        s[0] = 1.0;
        let before = s.len();
        reverb(&mut s, sr, 0.5, 0.4);
        assert!(s.len() > before, "reverb should lengthen the buffer");
        let tail: f32 = s[sr as usize / 4..].iter().map(|v| v.abs()).sum();
        assert!(tail > 0.0, "reverb produced no tail");
        assert!(
            s.iter().all(|v| v.is_finite()),
            "reverb produced NaN or inf"
        );
        assert!(
            s.iter().all(|v| v.abs() < 4.0),
            "reverb feedback is unstable"
        );
    }

    #[test]
    fn reverb_wet_gain_does_not_grow_with_room_size() {
        // A bigger room should be longer, not several times louder. Measured
        // with a SUSTAINED input, which is where comb feedback accumulates —
        // the impulse tests cannot see this.
        let sr = 8_000;
        let mut peaks = Vec::new();
        for room in [0.0f32, 0.5, 1.0] {
            let mut s = vec![1.0f32; sr as usize];
            reverb(&mut s, sr, room, 1.0);
            let peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak.is_finite(), "room {room} produced non-finite output");
            assert!(peak < 2.0, "room {room} reached {peak}, which will distort");
            peaks.push(peak);
        }
        let spread = peaks.iter().fold(0f32, |m, v| m.max(*v))
            - peaks.iter().fold(f32::MAX, |m, v| m.min(*v));
        assert!(
            spread < 0.5,
            "wet gain varies by {spread} across room sizes: {peaks:?}"
        );
    }

    #[test]
    fn reverb_with_zero_mix_is_a_no_op() {
        let mut a = vec![0.3, -0.2, 0.1, 0.0, 0.5];
        let b = a.clone();
        reverb(&mut a, 48_000, 0.5, 0.0);
        assert_eq!(a, b);
    }

    #[test]
    fn a_maximal_room_still_decays() {
        let sr = 48_000;
        let mut s = vec![0.0f32; sr as usize];
        s[0] = 1.0;
        reverb(&mut s, sr, 1.0, 1.0);
        assert!(s.iter().all(|v| v.is_finite()));
        assert!(s.iter().all(|v| v.abs() < 8.0), "feedback ran away");
    }

    #[test]
    fn soft_clip_is_bounded_and_monotonic() {
        assert!(soft_clip(100.0) <= 1.0);
        assert!(soft_clip(-100.0) >= -1.0);
        assert!(soft_clip(0.1) < soft_clip(0.2));
        assert!(soft_clip(0.0).abs() < 1e-6);
        assert!(soft_clip(f32::INFINITY).is_finite());
    }

    #[test]
    fn soft_clip_is_near_linear_at_low_levels() {
        // Quiet material must not be audibly distorted.
        assert!((soft_clip(0.05) - 0.05).abs() < 0.001);
    }
}
