//! Integration test: prove that the fill_buffer pattern (Audio::read → PcmRingBuffer::write)
//! must apply backpressure — every sample read from the decode pipeline must reach the
//! ring buffer, or the caller must stop reading until space is available.
//!
//! Without backpressure, `Audio::read()` consumes decoded data (advancing `samples_read`
//! / `position()`), but `PcmRingBuffer::write()` silently drops overflow.  Result:
//! position races ahead of actual audio output and playback skips fragments.

use std::time::Duration;

use kithara::audio::{Audio, AudioConfig};
use kithara::file::{FileConfig, FileSrc};
use kithara::stream::Stream;
use kithara_wasm::ring_buffer::PcmRingBuffer;
use rstest::rstest;

use kithara_test_utils::wav::create_test_wav;

/// Create an `Audio<Stream<File>>` pipeline from a WAV file.
///
/// Does NOT call `preload()` — reads block until data is available,
/// making the test deterministic (no timing dependency on decode worker).
async fn audio_from_wav(path: &std::path::Path) -> Audio<Stream<kithara::file::File>> {
    let file_config = FileConfig::new(FileSrc::Local(path.to_path_buf()));
    let config = AudioConfig::<kithara::file::File>::new(file_config).with_hint("wav");
    Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap()
}

/// Simulate `WasmPlayer::fill_buffer()` WITH backpressure:
///
///   1. Check `ring.space_available()` — how many samples the ring can accept
///   2. `audio.read(&mut pcm_buf[..space])` — only read what fits
///   3. `ring.write(&pcm_buf[..n])` — write all of it (guaranteed to fit)
///
/// Returns `(decoded_samples, samples_to_ring)`.
fn fill_buffer_with_backpressure(
    audio: &mut Audio<Stream<kithara::file::File>>,
    ring: &mut PcmRingBuffer,
    pcm_buf: &mut [f32],
) -> (usize, usize) {
    let space = ring.space_available() as usize;
    if space == 0 {
        return (0, 0);
    }
    let read_len = space.min(pcm_buf.len());

    let n = audio.read(&mut pcm_buf[..read_len]);
    if n == 0 {
        return (0, 0);
    }
    let written = ring.write(&pcm_buf[..n]);
    (n, written)
}

/// Backpressure: every sample read from the decoder reaches the ring buffer.
///
/// Parameterized over ring capacity to verify backpressure works regardless
/// of the ratio between ring size and PCM transfer buffer:
/// - `ring_smaller`: ring (1024) < buf (4096) — overflow on first read without backpressure
/// - `ring_equal`:   ring (4096) = buf (4096) — fits exactly once, then stops
/// - `ring_larger`:  ring (8192) > buf (4096) — multiple reads before full
#[rstest]
#[case::ring_smaller(1024, 4096)]
#[case::ring_equal(4096, 4096)]
#[case::ring_larger(8192, 4096)]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn fill_buffer_no_sample_loss(#[case] ring_capacity: usize, #[case] pcm_buf_size: usize) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    // 2 seconds of stereo audio at 44100 Hz.
    let wav = create_test_wav(88200, 44100, 2);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut std::fs::File::create(tmp.path()).unwrap(), &wav).unwrap();

    let mut audio = audio_from_wav(tmp.path()).await;
    let spec = audio.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, 44100);

    let mut ring = PcmRingBuffer::new(ring_capacity, spec.channels.into(), spec.sample_rate);
    let mut pcm_buf = vec![0.0f32; pcm_buf_size];

    let mut total_decoded = 0usize;
    let mut total_to_ring = 0usize;

    // Simulate 20 fill_buffer calls WITHOUT consuming from the ring
    // (= AudioWorklet not running / not keeping up).
    // With backpressure, stops reading once ring is full.
    for _ in 0..20 {
        let (decoded, written) = fill_buffer_with_backpressure(&mut audio, &mut ring, &mut pcm_buf);
        if decoded == 0 {
            break;
        }
        total_decoded += decoded;
        total_to_ring += written;
    }

    assert!(
        total_decoded > 0,
        "test setup error: must decode at least some samples"
    );

    // The invariant that MUST hold for correct playback:
    // every sample consumed from the decoder reaches the ring buffer.
    assert_eq!(
        total_decoded,
        total_to_ring,
        "backpressure violation: decoder produced {total_decoded} samples \
         but only {total_to_ring} reached the ring buffer — \
         {} samples lost (position will drift {:.1}x ahead of audio output)",
        total_decoded - total_to_ring,
        total_decoded as f64 / total_to_ring.max(1) as f64
    );
}

/// Verify that position tracks actual ring buffer content, not decode speed.
///
/// Parameterized over ring capacity — position must never exceed ring content
/// regardless of buffer geometry.
#[rstest]
#[case::ring_smaller(1024, 4096)]
#[case::ring_equal(4096, 4096)]
#[case::ring_larger(8192, 4096)]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn position_matches_ring_content(#[case] ring_capacity: usize, #[case] pcm_buf_size: usize) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    let wav = create_test_wav(88200, 44100, 2);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut std::fs::File::create(tmp.path()).unwrap(), &wav).unwrap();

    let mut audio = audio_from_wav(tmp.path()).await;
    let spec = audio.spec();

    let mut ring = PcmRingBuffer::new(ring_capacity, spec.channels.into(), spec.sample_rate);
    let mut pcm_buf = vec![0.0f32; pcm_buf_size];

    // Fill ring to capacity (no consumer). With backpressure, stops when full.
    for _ in 0..20 {
        let (decoded, _) = fill_buffer_with_backpressure(&mut audio, &mut ring, &mut pcm_buf);
        if decoded == 0 {
            break;
        }
    }

    let position = audio.position();
    let ring_samples = ring.available();
    let rate = f64::from(spec.sample_rate) * f64::from(spec.channels);
    let ring_duration = Duration::from_secs_f64(f64::from(ring_samples) / rate);

    // Position must not exceed what the ring buffer actually holds.
    // With backpressure, position ≈ ring_duration.
    // Without it, position >> ring_duration (the bug).
    let drift = position.as_secs_f64() / ring_duration.as_secs_f64().max(0.001);
    assert!(
        drift < 1.05,
        "position drifted {drift:.1}x ahead of ring buffer content: \
         position={position:?} but ring holds only {ring_duration:?} of audio"
    );
}
