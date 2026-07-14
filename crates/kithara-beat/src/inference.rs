use std::collections::HashMap;

use num_traits::cast::ToPrimitive;

use crate::{
    api::BeatError,
    runtime::{RtenModel, Tensor},
};

/// Chunking geometry for beat prediction.
struct Consts;

impl Consts {
    /// Frames discarded from each edge of predictions.
    const BORDER_SIZE: i64 = 6;
    /// Frames per chunk (30 seconds at 50 fps).
    const CHUNK_SIZE: i64 = 1500;
    /// Effective step between chunks.
    const STRIDE: i64 = Self::CHUNK_SIZE - 2 * Self::BORDER_SIZE;
}

fn as_i64(x: usize) -> i64 {
    x.to_i64().unwrap_or(i64::MAX)
}

fn as_usize(x: i64) -> usize {
    x.to_usize().unwrap_or(0)
}

/// Runs chunked beat/downbeat prediction on a mel spectrogram.
///
/// The beat model was trained on 1500-frame segments; longer audio is split
/// into overlapping chunks aggregated `keep_first` (earlier chunks win).
pub(crate) struct BeatPredictor {
    model: RtenModel,
}

/// Load the beat model from ONNX bytes.
impl TryFrom<&[u8]> for BeatPredictor {
    type Error = BeatError;

    fn try_from(bytes: &[u8]) -> Result<Self, BeatError> {
        Ok(Self {
            model: RtenModel::try_from(("beat", bytes))?,
        })
    }
}

impl BeatPredictor {
    /// Predict beats from a full mel spectrogram `[1, T, 128]`.
    ///
    /// Returns `(beat_logits, downbeat_logits)`, each of length T.
    pub(crate) fn predict(&mut self, mel: &Tensor) -> Result<(Vec<f32>, Vec<f32>), BeatError> {
        if mel.shape.len() != 3 || mel.shape[0] != 1 || mel.shape[2] != 128 {
            return Err(BeatError::Inference {
                reason: format!("expected mel shape [1, T, 128], got {:?}", mel.shape),
            });
        }

        let full_time = mel.shape[1];
        let starts = generate_starts(full_time);
        let border = as_usize(Consts::BORDER_SIZE);

        // Sentinel init; every frame is overwritten by some chunk.
        let mut beat_logits = vec![-1000.0f32; full_time];
        let mut downbeat_logits = vec![-1000.0f32; full_time];

        // Reverse order implements keep_first: earlier chunks are written
        // last and overwrite later chunks in overlapping regions.
        for &start in starts.iter().rev() {
            let chunk = extract_chunk(mel, start);
            let chunk_time = chunk.shape[1];

            let mut outputs = self.model.run(&[("spectrogram", &chunk)])?;

            let beat = extract_output(&mut outputs, "beat", "beat_logits")?;
            let downbeat = extract_output(&mut outputs, "downbeat", "downbeat_logits")?;

            let valid_beat = &beat.data[border..chunk_time - border];
            let valid_downbeat = &downbeat.data[border..chunk_time - border];

            // start >= -BORDER_SIZE, so this is non-negative.
            let write_start = as_usize(start + Consts::BORDER_SIZE);
            for (i, (&b, &d)) in valid_beat.iter().zip(valid_downbeat.iter()).enumerate() {
                let dest = write_start + i;
                if dest < full_time {
                    beat_logits[dest] = b;
                    downbeat_logits[dest] = d;
                }
            }
        }

        Ok((beat_logits, downbeat_logits))
    }
}

/// Extract a named output tensor, trying a fallback ONNX export name.
fn extract_output(
    outputs: &mut HashMap<String, Tensor>,
    primary: &str,
    fallback: &str,
) -> Result<Tensor, BeatError> {
    if let Some(t) = outputs.remove(primary) {
        return Ok(t);
    }
    if let Some(t) = outputs.remove(fallback) {
        return Ok(t);
    }
    Err(BeatError::Inference {
        reason: format!(
            "model missing output '{primary}' (also tried '{fallback}'); available: {:?}",
            outputs.keys()
        ),
    })
}

/// Generate chunk start positions for a spectrogram of `full_time` frames.
///
/// Starts at `-BORDER_SIZE`, steps by `STRIDE`, and adjusts the last start
/// to align with the spectrogram end (`avoid_short_end`).
fn generate_starts(full_time: usize) -> Vec<i64> {
    let full_time = as_i64(full_time);
    let mut starts = Vec::new();
    let mut pos = -Consts::BORDER_SIZE;
    let limit = full_time - Consts::BORDER_SIZE;

    while pos < limit {
        starts.push(pos);
        pos += Consts::STRIDE;
    }

    if full_time > Consts::STRIDE
        && let Some(last) = starts.last_mut()
    {
        *last = full_time - (Consts::CHUNK_SIZE - Consts::BORDER_SIZE);
    }

    starts
}

/// Extract a single chunk from the mel spectrogram, zero-padding as needed.
///
/// The right padding is capped at `BORDER_SIZE`:
/// `right = max(0, min(border_size, start + chunk_size - len(spect)))`.
fn extract_chunk(mel: &Tensor, start: i64) -> Tensor {
    let full_time = mel.shape[1];
    let n_mels = mel.shape[2];
    let full_time_i = as_i64(full_time);

    let actual_start = as_usize(start.max(0));
    let actual_end = as_usize((start + Consts::CHUNK_SIZE).min(full_time_i));
    let pad_left = as_usize((-start).max(0));
    let n_frames = actual_end - actual_start;

    let pad_right =
        as_usize(0.max((start + Consts::CHUNK_SIZE - full_time_i).min(Consts::BORDER_SIZE)));

    let chunk_time = pad_left + n_frames + pad_right;
    let mut data = vec![0.0f32; chunk_time * n_mels];

    for t in actual_start..actual_end {
        let src_offset = t * n_mels;
        let dst_t = pad_left + (t - actual_start);
        let dst_offset = dst_t * n_mels;
        data[dst_offset..dst_offset + n_mels]
            .copy_from_slice(&mel.data[src_offset..src_offset + n_mels]);
    }

    Tensor {
        data,
        shape: vec![1, chunk_time, n_mels],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_starts_short() {
        // 100 frames — fits in a single chunk.
        let starts = generate_starts(100);
        assert_eq!(starts, vec![-6]);
    }

    #[test]
    fn generate_starts_exact_chunk() {
        // Exactly 1500 frames — needs 2 chunks: after border trimming the
        // first chunk only covers frames 0..1488.
        let starts = generate_starts(1500);
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], -6);
        assert_eq!(starts[1], 6);
    }

    #[test]
    fn generate_starts_two_chunks() {
        let starts = generate_starts(2000);
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], -6);
        // Last start adjusted: 2000 - (1500 - 6) = 506.
        assert_eq!(starts[1], 506);
    }

    #[test]
    fn generate_starts_long() {
        let starts = generate_starts(5000);
        assert_eq!(starts[0], -6);
        // Stride 1488: -6, 1482, 2970, last adjusted to 5000 - 1494 = 3506.
        assert_eq!(starts.len(), 4);
        assert_eq!(starts[1], 1482);
        assert_eq!(starts[2], 2970);
        assert_eq!(starts[3], 3506);
    }

    #[test]
    fn generate_starts_coverage() {
        // Every frame is covered by at least one chunk after border trimming.
        for full_time in [50usize, 100, 500, 1488, 1500, 2000, 3000, 5000, 7800] {
            let starts = generate_starts(full_time);
            let full_time_i = as_i64(full_time);
            let mut covered = vec![false; full_time];
            for &start in &starts {
                let pad_left = (-start).max(0);
                let actual_end = (start + Consts::CHUNK_SIZE).min(full_time_i);
                let actual_start = start.max(0);
                let n_frames = actual_end - actual_start;
                let pad_right =
                    0.max((start + Consts::CHUNK_SIZE - full_time_i).min(Consts::BORDER_SIZE));
                let chunk_time = pad_left + n_frames + pad_right;
                let write_start = as_usize((start + Consts::BORDER_SIZE).max(0));
                let write_end =
                    as_usize((start + chunk_time - Consts::BORDER_SIZE).min(full_time_i));
                for c in &mut covered[write_start..write_end] {
                    *c = true;
                }
            }
            assert!(
                covered.iter().all(|&c| c),
                "not all frames covered for full_time={full_time}; first uncovered: {:?}",
                covered.iter().position(|&c| !c)
            );
        }
    }

    #[test]
    fn extract_chunk_short_audio() {
        // Short audio (100 frames < STRIDE): pad_left 6 + 100 + pad_right 6 = 112.
        let n_mels = 128;
        let full_time = 100;
        let mel = Tensor {
            shape: vec![1, full_time, n_mels],
            data: vec![1.0; full_time * n_mels],
        };

        let chunk = extract_chunk(&mel, -6);
        assert_eq!(chunk.shape, vec![1, 112, n_mels]);

        for t in 0..6 {
            assert_eq!(
                chunk.data[t * n_mels],
                0.0,
                "expected zero padding at t={t}"
            );
        }
        assert_eq!(chunk.data[6 * n_mels], 1.0);
        assert_eq!(chunk.data[105 * n_mels], 1.0);
        for t in 106..112 {
            assert_eq!(
                chunk.data[t * n_mels],
                0.0,
                "expected zero padding at t={t}"
            );
        }
    }

    #[test]
    fn extract_chunk_long_audio_first() {
        let n_mels = 128;
        let full_time = 5000;
        let mel = Tensor {
            shape: vec![1, full_time, n_mels],
            data: vec![1.0; full_time * n_mels],
        };

        let chunk = extract_chunk(&mel, -6);
        assert_eq!(chunk.shape, vec![1, as_usize(Consts::CHUNK_SIZE), n_mels]);

        for t in 0..6 {
            assert_eq!(chunk.data[t * n_mels], 0.0);
        }
        assert_eq!(chunk.data[6 * n_mels], 1.0);
    }

    #[test]
    fn extract_chunk_long_audio_middle() {
        let n_mels = 128;
        let full_time = 5000;
        let mel = Tensor {
            shape: vec![1, full_time, n_mels],
            data: vec![1.0; full_time * n_mels],
        };

        let chunk = extract_chunk(&mel, 100);
        assert_eq!(chunk.shape, vec![1, as_usize(Consts::CHUNK_SIZE), n_mels]);
        assert!(chunk.data.iter().all(|&v| v == 1.0));
    }
}
