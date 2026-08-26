//! Scientific pitch notation, so pack authors write `"C5"` instead of `523.25`.
//!
//! A pack is meant to be readable and reviewable as a text diff. `notes =
//! ["C5", "E5", "G5"]` communicates "a major triad"; three float literals do
//! not.

/// Concert pitch. A4 = 440 Hz.
const A4_HZ: f64 = 440.0;
/// MIDI note number of A4.
const A4_MIDI: i32 = 69;
const SEMITONES_PER_OCTAVE: f64 = 12.0;

/// Parse scientific pitch notation to Hz.
///
/// Accepts `C4`, `A#3`, `Eb5`, `As3` (a typo-tolerant sharp), lowercase, and
/// octaves `-1` through `9`. Returns `None` rather than guessing, so a typo in
/// a pack manifest becomes silence-for-that-layer instead of a wrong note.
pub fn note_to_hz(name: &str) -> Option<f64> {
    let mut chars = name.trim().chars();

    // Semitone offset of each natural within its octave, C = 0.
    let base: i32 = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };

    let rest: String = chars.collect();
    let (accidental, octave_part) = match rest.chars().next() {
        // `s` for sharp: `A#3` in TOML is fine, but `As3` survives careless quoting.
        Some('#' | 's' | 'S') => (1, &rest[1..]),
        Some('b' | 'B') => (-1, &rest[1..]),
        _ => (0, rest.as_str()),
    };

    let octave: i32 = octave_part.parse().ok()?;
    if !(-1..=9).contains(&octave) {
        return None;
    }

    let midi = 12 * (octave + 1) + base + accidental;
    Some(A4_HZ * 2f64.powf(f64::from(midi - A4_MIDI) / SEMITONES_PER_OCTAVE))
}

/// Shift a frequency by a (possibly fractional) number of semitones.
pub fn transpose(hz: f64, semitones: f32) -> f64 {
    hz * 2f64.powf(f64::from(semitones) / SEMITONES_PER_OCTAVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 0.01, "{a} != {b}");
    }

    #[test]
    fn reference_pitches() {
        close(note_to_hz("A4").unwrap(), 440.0);
        close(note_to_hz("C4").unwrap(), 261.63);
        close(note_to_hz("C5").unwrap(), 523.25);
        close(note_to_hz("E5").unwrap(), 659.26);
        close(note_to_hz("G5").unwrap(), 783.99);
    }

    #[test]
    fn accidentals_are_enharmonically_equivalent() {
        close(note_to_hz("A#3").unwrap(), note_to_hz("Bb3").unwrap());
        close(note_to_hz("Eb5").unwrap(), note_to_hz("D#5").unwrap());
        close(note_to_hz("As3").unwrap(), note_to_hz("A#3").unwrap());
    }

    #[test]
    fn octaves_double_the_frequency() {
        close(note_to_hz("A5").unwrap(), 880.0);
        close(note_to_hz("A3").unwrap(), 220.0);
        close(note_to_hz("A0").unwrap(), 27.5);
    }

    #[test]
    fn case_is_ignored_for_the_letter() {
        close(note_to_hz("c4").unwrap(), note_to_hz("C4").unwrap());
        close(note_to_hz("g5").unwrap(), note_to_hz("G5").unwrap());
    }

    #[test]
    fn the_lowest_and_highest_octaves_parse() {
        assert!(note_to_hz("C-1").is_some());
        assert!(note_to_hz("G9").is_some());
    }

    #[test]
    fn nonsense_returns_none_rather_than_a_wrong_pitch() {
        for bad in [
            "", "H4", "C", "4C", "C99", "C4x", "#4", "Cbb4", "C10", "C-2", " ", "-",
        ] {
            assert!(note_to_hz(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        close(note_to_hz(" A4 ").unwrap(), 440.0);
    }

    #[test]
    fn transpose_by_an_octave_doubles_or_halves() {
        close(transpose(440.0, 12.0), 880.0);
        close(transpose(440.0, -12.0), 220.0);
        close(transpose(440.0, 0.0), 440.0);
    }

    #[test]
    fn transpose_accepts_fractional_semitones() {
        close(transpose(440.0, 0.5), 440.0 * 2f64.powf(0.5 / 12.0));
    }

    #[test]
    fn transposition_composes() {
        close(transpose(transpose(440.0, 7.0), 5.0), 880.0);
    }
}
