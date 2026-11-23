//! Sample format conversion from the audio source's mix format to the card display's sound output's. No Windows calls.

/// How one sample is stored. The shared-mode mix format is almost always `F32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    F32,
    I16,
    /// Packed 3-byte little-endian.
    I24,
    I32,
}

impl Sample {
    /// From a wave format's float/PCM encoding and container size (`wBitsPerSample`).
    pub fn from_wave(float: bool, bits: u16) -> Option<Self> {
        match (float, bits) {
            (true, 32) => Some(Self::F32),
            (false, 16) => Some(Self::I16),
            (false, 24) => Some(Self::I24),
            (false, 32) => Some(Self::I32),
            _ => None,
        }
    }

    fn read(self, bytes: &[u8]) -> f32 {
        match (self, bytes) {
            (Self::F32, &[a, b, c, d, ..]) => f32::from_le_bytes([a, b, c, d]),
            (Self::I16, &[a, b, ..]) => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
            // Shifting the three bytes into the top of an i32 keeps the sign.
            (Self::I24, &[a, b, c, ..]) => {
                (i32::from_le_bytes([0, a, b, c]) >> 8) as f32 / 8_388_608.0
            }
            (Self::I32, &[a, b, c, d, ..]) => {
                i32::from_le_bytes([a, b, c, d]) as f32 / 2_147_483_648.0
            }
            _ => 0.0,
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is clamped to -1..=1 first, so the scaled value fits the integer"
    )]
    fn write(self, value: f32, out: &mut Vec<u8>) {
        let value = value.clamp(-1.0, 1.0);
        match self {
            Self::F32 => out.extend_from_slice(&value.to_le_bytes()),
            Self::I16 => out.extend_from_slice(&((value * 32_767.0).round() as i16).to_le_bytes()),
            Self::I24 => {
                out.extend_from_slice(&((value * 8_388_607.0).round() as i32).to_le_bytes()[..3])
            }
            Self::I32 => out.extend_from_slice(
                &((f64::from(value) * 2_147_483_647.0).round() as i32).to_le_bytes(),
            ),
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::F32 | Self::I32 => 4,
        }
    }
}

/// An interleaved stream format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub sample: Sample,
    pub channels: u16,
    pub rate: u32,
}

impl Format {
    pub fn frame_bytes(self) -> usize {
        self.sample.bytes() * usize::from(self.channels)
    }
}

/// Appends `input` (whole frames in `from`) to `out` as frames in `to`. The sample rate is not
/// changed here; the render stream's rate adjustment does that.
pub fn convert(input: &[u8], from: Format, to: Format, out: &mut Vec<u8>) {
    if (from.sample, from.channels) == (to.sample, to.channels) {
        out.extend_from_slice(input);
        return;
    }
    let size = from.sample.bytes();
    let mut frame = vec![0.0; usize::from(from.channels)];
    let mut mixed = vec![0.0; usize::from(to.channels)];
    for bytes in input.chunks_exact(from.frame_bytes()) {
        for (sample, bytes) in frame.iter_mut().zip(bytes.chunks_exact(size)) {
            *sample = from.sample.read(bytes);
        }
        mix(&frame, &mut mixed);
        for &sample in &mixed {
            to.sample.write(sample, out);
        }
    }
}

/// -3 dB, the usual level for folding the centre and surrounds into stereo.
const FOLD: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Maps one frame between channel counts, in the standard WAVE order (front left, front right,
/// centre, LFE, then surround pairs).
fn mix(input: &[f32], out: &mut [f32]) {
    out.fill(0.0);
    match (input, out.len()) {
        ([], _) => {}
        ([mono], _) => out.iter_mut().take(2).for_each(|sample| *sample = *mono),
        ([left, right, ..], 1) => out[0] = (left + right) / 2.0,
        ([left, right, centre, _lfe, surrounds @ ..], 2) => {
            let (mut out_left, mut out_right) = (left + FOLD * centre, right + FOLD * centre);
            for pair in surrounds.chunks(2) {
                out_left += FOLD * pair[0];
                out_right += FOLD * pair.get(1).copied().unwrap_or(0.0);
            }
            out[0] = out_left;
            out[1] = out_right;
        }
        _ => out
            .iter_mut()
            .zip(input)
            .for_each(|(out, input)| *out = *input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(sample: Sample, channels: u16) -> Format {
        Format {
            sample,
            channels,
            rate: 48_000,
        }
    }

    fn f32_bytes(samples: &[f32]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    fn as_f32(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect()
    }

    fn converted(input: &[u8], from: Format, to: Format) -> Vec<u8> {
        let mut out = Vec::new();
        convert(input, from, to, &mut out);
        out
    }

    #[test]
    fn wave_encodings_map_to_samples() {
        assert_eq!(Sample::from_wave(true, 32), Some(Sample::F32));
        assert_eq!(Sample::from_wave(false, 16), Some(Sample::I16));
        assert_eq!(Sample::from_wave(false, 24), Some(Sample::I24));
        assert_eq!(Sample::from_wave(false, 32), Some(Sample::I32));
        assert_eq!(Sample::from_wave(true, 64), None);
        assert_eq!(Sample::from_wave(false, 8), None);
    }

    #[test]
    fn same_format_is_copied_unchanged() {
        let input = f32_bytes(&[0.25, -0.5, 1.0, 0.0]);
        let stereo = format(Sample::F32, 2);
        assert_eq!(converted(&input, stereo, stereo), input);
    }

    #[test]
    fn float_becomes_16_bit_and_clips() {
        let input = f32_bytes(&[0.5, -1.0, 1.5, -2.0]);
        let out = converted(&input, format(Sample::F32, 2), format(Sample::I16, 2));
        let samples: Vec<i16> = out
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| i16::from_le_bytes(*chunk))
            .collect();
        assert_eq!(samples, [16384, -32767, 32767, -32767]);
    }

    #[test]
    fn integer_sources_become_float() {
        let to = format(Sample::F32, 1);
        assert_eq!(
            as_f32(&converted(
                &i16::MIN.to_le_bytes(),
                format(Sample::I16, 1),
                to
            )),
            [-1.0]
        );
        assert_eq!(
            as_f32(&converted(&[0x00, 0x00, 0x40], format(Sample::I24, 1), to)),
            [0.5]
        );
        assert_eq!(
            as_f32(&converted(&[0x00, 0x00, 0xC0], format(Sample::I24, 1), to)),
            [-0.5]
        );
        assert_eq!(
            as_f32(&converted(
                &0x4000_0000_i32.to_le_bytes(),
                format(Sample::I32, 1),
                to
            )),
            [0.5]
        );
    }

    #[test]
    fn mono_source_plays_on_both_sides() {
        let out = converted(
            &f32_bytes(&[0.25, -0.5]),
            format(Sample::F32, 1),
            format(Sample::F32, 2),
        );
        assert_eq!(as_f32(&out), [0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn stereo_source_on_a_surround_output_fills_front_left_and_right() {
        let out = converted(
            &f32_bytes(&[0.25, -0.5]),
            format(Sample::F32, 2),
            format(Sample::F32, 6),
        );
        assert_eq!(as_f32(&out), [0.25, -0.5, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn surround_source_is_mixed_down_to_stereo_keeping_the_centre() {
        // 5.1 order: front left, front right, centre, LFE, surround left, surround right.
        let out = converted(
            &f32_bytes(&[0.2, 0.0, 0.4, 1.0, 0.2, 0.0]),
            format(Sample::F32, 6),
            format(Sample::F32, 2),
        );
        let out = as_f32(&out);
        // Centre and surrounds at -3 dB (0.7071), LFE dropped.
        assert!((out[0] - 0.624_26).abs() < 1e-4, "{out:?}");
        assert!((out[1] - 0.282_84).abs() < 1e-4, "{out:?}");
    }
}
