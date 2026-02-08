/// Test support utilities for kithara-decode.
///
/// Available under `#[cfg(test)]` and optionally via the `test-utils` feature.

/// Create minimal valid WAV file (PCM 16-bit).
pub fn create_test_wav(sample_count: usize, sample_rate: u32, channels: u16) -> Vec<u8> {
    let bytes_per_sample = 2; // 16-bit = 2 bytes
    let data_size = (sample_count * channels as usize * bytes_per_sample) as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::new();

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = channels * bytes_per_sample as u16;
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    // Generate simple sine wave samples
    for i in 0..sample_count {
        let sample = ((i as f32 * 0.1).sin() * 32767.0) as i16;
        for _ in 0..channels {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }

    wav
}
