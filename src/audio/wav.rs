//! Minimal 16-bit PCM WAV encoding.
//!
//! Needed for two things: handing a temp file to a system player when the
//! embedded backend is unavailable, and letting a pack author hear what they
//! just wrote.

use crate::audio::synth::Pcm;
use std::path::Path;

const HEADER_BYTES: u32 = 44;
const BITS_PER_SAMPLE: u16 = 16;

/// Encode to a complete WAV file, header included.
pub fn encode(pcm: &Pcm) -> Vec<u8> {
    let channels = pcm.channels.max(1);
    let bytes_per_sample = u32::from(BITS_PER_SAMPLE / 8);
    let data_len = (pcm.samples.len() as u32) * bytes_per_sample;
    let byte_rate = pcm.sample_rate * u32::from(channels) * bytes_per_sample;
    let block_align = channels * (BITS_PER_SAMPLE / 8);

    let mut out = Vec::with_capacity(HEADER_BYTES as usize + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(HEADER_BYTES - 8 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // uncompressed PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&pcm.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in &pcm.samples {
        out.extend_from_slice(&to_i16(*sample).to_le_bytes());
    }
    out
}

pub fn write(path: &Path, pcm: &Pcm) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, encode(pcm))
}

/// Clamp before scaling. `i16::MIN` is one louder than `i16::MAX`, so scaling
/// by 32768 would wrap a full-scale positive sample to full-scale negative —
/// an extremely audible click.
fn to_i16(sample: f32) -> i16 {
    let clamped = if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (clamped * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: Vec<f32>) -> Pcm {
        Pcm {
            sample_rate: 48_000,
            channels: 2,
            samples,
        }
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn the_header_is_a_valid_riff_wave() {
        let bytes = encode(&pcm(vec![0.0; 8]));
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[36..40], b"data");
    }

    #[test]
    fn the_declared_sizes_match_the_actual_payload() {
        let bytes = encode(&pcm(vec![0.0; 10]));
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8, "RIFF size");
        assert_eq!(u32_at(&bytes, 40) as usize, bytes.len() - 44, "data size");
        assert_eq!(bytes.len(), 44 + 10 * 2);
    }

    #[test]
    fn format_fields_describe_the_pcm() {
        let bytes = encode(&pcm(vec![0.0; 4]));
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(u32_at(&bytes, 24), 48_000);
        assert_eq!(u32_at(&bytes, 28), 48_000 * 2 * 2, "byte rate");
        assert_eq!(
            u16::from_le_bytes(bytes[32..34].try_into().unwrap()),
            4,
            "block align"
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
    }

    #[test]
    fn samples_round_trip_through_sixteen_bit() {
        let bytes = encode(&pcm(vec![0.0, 1.0, -1.0, 0.5]));
        let decoded: Vec<i16> = bytes[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(decoded[0], 0);
        assert_eq!(decoded[1], i16::MAX);
        assert_eq!(decoded[2], -i16::MAX);
        assert!((decoded[3] - 16_384).abs() < 2, "got {}", decoded[3]);
    }

    #[test]
    fn full_scale_positive_does_not_wrap_to_negative() {
        // Scaling by 32768 instead of 32767 wraps +1.0 to i16::MIN, which is a
        // loud click exactly where the sound is loudest.
        let bytes = encode(&pcm(vec![1.0, 1.2, 5.0]));
        let decoded: Vec<i16> = bytes[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert!(
            decoded.iter().all(|s| *s > 0),
            "wrapped to negative: {decoded:?}"
        );
    }

    #[test]
    fn non_finite_samples_become_silence_rather_than_noise() {
        let bytes = encode(&pcm(vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY]));
        let decoded: Vec<i16> = bytes[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(decoded, vec![0, 0, 0]);
    }

    #[test]
    fn an_empty_pcm_still_produces_a_valid_header() {
        let bytes = encode(&pcm(vec![]));
        assert_eq!(bytes.len(), 44);
        assert_eq!(u32_at(&bytes, 40), 0);
    }

    #[test]
    fn writing_creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/tone.wav");
        write(&path, &pcm(vec![0.0; 4])).unwrap();
        assert_eq!(std::fs::read(&path).unwrap().len(), 44 + 8);
    }
}
