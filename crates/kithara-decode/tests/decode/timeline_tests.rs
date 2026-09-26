use std::io::Cursor;

use kithara_decode::{DecoderConfig, DecoderFactory};
use kithara_platform::time::Duration;
use kithara_resampler::NoResamplerBackend;
use kithara_signal::AudioChunk;
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};

type TestDecoderConfig = DecoderConfig<NoResamplerBackend, TestPools>;

use kithara_test_fixtures::fixtures::tone_wav;
#[kithara::test]
fn test_progressive_file_timeline_monotonic(tone_wav: &'static [u8]) {
    let reader = Cursor::new(tone_wav);

    let mut decoder = DecoderFactory::create_with_probe(
        reader,
        Some("wav"),
        TestDecoderConfig::builder().pools(pools()).build(),
    )
    .unwrap();

    let mut prev_frame_end = 0u64;
    let mut chunk_count = 0u64;

    while let Ok(kithara_decode::DecoderChunkOutcome::Chunk(chunk)) = decoder.next_chunk() {
        let meta = chunk.meta;

        assert_eq!(meta.spec.sample_rate.get(), 44100);
        assert_eq!(meta.spec.channels, 2);

        assert_eq!(
            meta.frame_offset, prev_frame_end,
            "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
            meta.frame_offset
        );

        let expected_ts = Duration::from_secs_f64(
            meta.frame_offset as f64 / f64::from(meta.spec.sample_rate.get()),
        );
        let diff = meta.timestamp.abs_diff(expected_ts);
        assert!(
            diff < Duration::from_micros(100),
            "timestamp drift: {diff:?}"
        );

        assert_eq!(meta.segment_index, None);
        assert_eq!(meta.variant_index, None);

        assert_eq!(meta.epoch, 0);

        prev_frame_end = meta.frame_offset + chunk.frames() as u64;
        chunk_count += 1;
    }

    assert!(chunk_count > 0, "should have decoded some chunks");
}

#[kithara::test]
fn test_progressive_file_seek_resets_frame_offset(tone_wav: &'static [u8]) {
    let reader = Cursor::new(tone_wav);

    let mut decoder = DecoderFactory::create_with_probe(
        reader,
        Some("wav"),
        TestDecoderConfig::builder().pools(pools()).build(),
    )
    .unwrap();

    for _ in 0..3 {
        let _ = decoder.next_chunk();
    }

    decoder.seek(Duration::from_millis(500)).unwrap();

    let chunk = AudioChunk::try_from(decoder.next_chunk().unwrap()).unwrap();
    let expected_frame = num_traits::cast::<f64, u64>(0.5 * 44100.0).unwrap_or(u64::MAX);

    let diff = (chunk.meta.frame_offset as i64 - expected_frame as i64).unsigned_abs();
    assert!(
        diff < 2048,
        "frame_offset after seek: {} expected ~{}",
        chunk.meta.frame_offset,
        expected_frame
    );
}
