//! Common WAV file generation helpers for integration tests.

/// Create a WAV file with sine wave samples.
///
/// Parameters:
/// - `sample_count`: number of audio frames
/// - `sample_rate`: e.g. 44100
/// - `channels`: e.g. 2 for stereo
pub fn create_test_wav(sample_count: usize, sample_rate: u32, channels: u16) -> Vec<u8> {
    let bytes_per_sample = 2; // 16-bit
    let data_size = (sample_count * channels as usize * bytes_per_sample) as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::with_capacity(file_size as usize + 8);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
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

    // Generate sine wave samples
    for i in 0..sample_count {
        let sample = ((i as f32 * 0.1).sin() * 32767.0) as i16;
        for _ in 0..channels {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }

    wav
}

/// Create WAV with saw-tooth pattern, sized exactly to `total_bytes`.
///
/// Stereo 44100 Hz, 16-bit PCM. L and R channels get the same value per frame.
pub fn create_saw_wav(total_bytes: usize) -> Vec<u8> {
    const SAW_PERIOD: usize = 65536;
    const SAMPLE_RATE: u32 = 44100;
    const CHANNELS: u16 = 2;

    let bytes_per_sample: u16 = 2;
    let bytes_per_frame = CHANNELS as usize * bytes_per_sample as usize;
    let header_size = 44usize;
    let data_size = total_bytes - header_size;
    let frame_count = data_size / bytes_per_frame;
    let data_size = (frame_count * bytes_per_frame) as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::with_capacity(total_bytes);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * bytes_per_sample as u32;
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = CHANNELS * bytes_per_sample;
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    // PCM data: saw-tooth
    for i in 0..frame_count {
        let sample = ((i % SAW_PERIOD) as i32 - 32768) as i16;
        for _ in 0..CHANNELS {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }

    wav.resize(total_bytes, 0);
    wav
}
