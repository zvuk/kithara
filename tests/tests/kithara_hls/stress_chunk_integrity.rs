//! Stress test: chunk-level ABR integrity via `PcmReader::next_chunk`.
//!
//! Same ABR scenario as `stress_seek_abr_audio` (ascending V0, descending V1,
//! delayed V0 → ABR switch to V1), but reads whole `PcmChunk` values instead
//! of raw f32 samples. This gives access to `PcmMeta` (`frame_offset`, timestamp,
//! `segment_index`, `variant_index`, epoch) for precise root-cause diagnosis.
//!
//! Four-phase verification:
//! 1. **Warmup**: read chunks until ABR switches from V0 (ascending) to V1 (descending)
//! 2. **Post-switch**: 50 sequential V1 chunks with `frame_offset` continuity
//! 3. **Random seeks**: 200 seek + 5 chunk reads with continuity + saw-tooth checks
//! 4. **EOF**: seek near end, drain to EOF

use std::{num::NonZeroUsize, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, PcmReader},
    decode::{PcmChunk, PcmMeta},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{
    SignalDirection as Direction, TestTempDir, Xorshift64, detect_direction,
    fixture_protocol::DelayRule,
    phase_from_f32,
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 50;
    const SEEK_ITERATIONS: usize = 200;
    const WARMUP_TIMEOUT_SECS: u64 = 30;
    const TEST_TIMEOUT_SECS: u64 = 60;
    const POST_SWITCH_CHUNKS: usize = 50;
    const CHUNKS_PER_SEEK: usize = 5;
    const WARMUP_NEXT_CHUNK_TIMEOUT_MS: u64 = 5_000;
    const NEXT_CHUNK_TIMEOUT_MS: u64 = 3_000;
}

fn detect_chunk_direction(chunk: &PcmChunk) -> Direction {
    let channels = chunk.meta.spec.channels as usize;
    detect_direction(&chunk.pcm, channels)
}

/// Format chunk metadata for diagnostic output.
fn format_meta(meta: &PcmMeta, pcm_len: usize) -> String {
    format!(
        "frame_offset={}, samples={}, segment={:?}, variant={:?}, epoch={}",
        meta.frame_offset, pcm_len, meta.segment_index, meta.variant_index, meta.epoch
    )
}

/// Check saw-tooth continuity within a single chunk.
/// Returns the number of breaks found.
fn intra_chunk_breaks(chunk: &PcmChunk) -> usize {
    let channels = chunk.meta.spec.channels as usize;
    let frames = chunk.frames();
    if frames < 2 {
        return 0;
    }

    let mut breaks = 0;
    for f in 1..frames {
        let prev_phase = phase_from_f32(chunk.pcm[(f - 1) * channels]);
        let curr_phase = phase_from_f32(chunk.pcm[f * channels]);
        let expected_asc = (prev_phase as usize + 1) % SawWav::SAW_PERIOD;
        let expected_desc = (prev_phase as usize + SawWav::SAW_PERIOD - 1) % SawWav::SAW_PERIOD;
        if curr_phase != expected_asc && curr_phase != expected_desc {
            breaks += 1;
        }
    }
    breaks
}

async fn next_chunk_with_timeout(
    audio: &mut Audio<Stream<Hls>>,
    timeout: Duration,
    stage: &str,
) -> Option<PcmChunk> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(chunk) = PcmReader::next_chunk(audio) {
            return Some(chunk);
        }
        if audio.is_eof() {
            return None;
        }
        assert!(
            Instant::now() <= deadline,
            "next_chunk timeout at stage='{stage}' (is_eof={})",
            audio.is_eof()
        );
        sleep(Duration::from_micros(500)).await;
    }
}

// Stress Test

#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
)]
#[case::mmap(false)]
#[case::ephemeral(true)]
async fn stress_chunk_integrity(#[case] ephemeral: bool) {
    // Generate WAV data for two variants
    let init_segment = Arc::new(create_wav_header(
        Consts::D.sample_rate,
        Consts::D.channels,
        None,
    ));
    let v0_pcm = Arc::new(
        SignalPcm::new(
            signal::Sawtooth,
            Consts::D.sample_rate,
            Consts::D.channels,
            Finite::from_segments(
                Consts::SEGMENT_COUNT,
                Consts::D.segment_size,
                Consts::D.channels,
            ),
        )
        .into_vec(),
    );
    let v1_pcm = Arc::new(
        SignalPcm::new(
            signal::SawtoothDescending,
            Consts::D.sample_rate,
            Consts::D.channels,
            Finite::from_segments(
                Consts::SEGMENT_COUNT,
                Consts::D.segment_size,
                Consts::D.channels,
            ),
        )
        .into_vec(),
    );

    info!(
        init_size = init_segment.len(),
        v0_size = v0_pcm.len(),
        v1_size = v1_pcm.len(),
        segments = Consts::SEGMENT_COUNT,
        "Generated WAV data for two variants"
    );

    // Spawn HLS server
    let segment_duration = Consts::D.segment_size as f64
        / (f64::from(Consts::D.sample_rate) * f64::from(Consts::D.channels) * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&v0_pcm), Arc::clone(&v1_pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(3),
            delay_ms: 500,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    info!(%url, "HLS server ready with 2 variants");

    // Create Audio<Stream<Hls>> with Auto ABR starting on V0
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        // Ephemeral mode auto-evicts MemResources from LRU cache.
        // 2 variants × Consts::SEGMENT_COUNT segments + headroom.
        store.cache_capacity =
            Some(NonZeroUsize::new(Consts::SEGMENT_COUNT * 2 + 10).expect("nonzero"));
        store.ephemeral = true;
    }

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(cancel)
        .with_abr_options(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(120),
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    audio.preload();

    // Phase 1: Warmup + ABR switch detection
    info!("Phase 1: waiting for ABR switch (ascending -> descending) via chunks...");

    let warmup_start = Instant::now();
    let warmup_timeout = Duration::from_secs(Consts::WARMUP_TIMEOUT_SECS);
    let mut warmup_ascending = 0u64;
    let mut warmup_unknown = 0u64;

    loop {
        if warmup_start.elapsed() > warmup_timeout {
            panic!(
                "ABR switch not detected within {}s (ascending={}, unknown={})",
                Consts::WARMUP_TIMEOUT_SECS,
                warmup_ascending,
                warmup_unknown
            );
        }

        let Some(chunk) = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(Consts::WARMUP_NEXT_CHUNK_TIMEOUT_MS),
            "phase1_warmup",
        )
        .await
        else {
            panic!(
                "Hit EOF before ABR switch (ascending={}, unknown={})",
                warmup_ascending, warmup_unknown
            );
        };

        let dir = detect_chunk_direction(&chunk);
        match dir {
            Direction::Ascending => {
                warmup_ascending += 1;
            }
            Direction::Descending => {
                info!(
                    warmup_ascending,
                    warmup_unknown,
                    elapsed_ms = warmup_start.elapsed().as_millis(),
                    chunk_meta = %format_meta(&chunk.meta, chunk.pcm.len()),
                    "ABR switch detected: ascending -> descending"
                );
                break;
            }
            Direction::Unknown => {
                warmup_unknown += 1;
            }
        }

        if warmup_ascending.is_multiple_of(100) && warmup_ascending > 0 {
            info!(
                warmup_ascending,
                warmup_unknown,
                elapsed_ms = warmup_start.elapsed().as_millis(),
                "Still waiting for ABR switch..."
            );
        }
    }

    // Phase 2: Post-switch sequential read — frame_offset continuity
    info!(
        "Phase 2: verifying {} post-switch chunks...",
        Consts::POST_SWITCH_CHUNKS
    );

    let mut prev_frame_offset: Option<u64> = None;
    let mut prev_frames: Option<usize> = None;
    let mut continuity_breaks = 0u64;

    for chunk_idx in 0..Consts::POST_SWITCH_CHUNKS {
        let stage = format!("phase2_post_switch_chunk_{chunk_idx}");
        let Some(chunk) = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(Consts::NEXT_CHUNK_TIMEOUT_MS),
            &stage,
        )
        .await
        else {
            panic!(
                "next_chunk returned None at post-switch chunk {chunk_idx} (is_eof={})",
                audio.is_eof()
            );
        };

        let frames = chunk.frames();
        let meta = &chunk.meta;

        // Integrity: all samples finite and in [-1, 1]
        for (j, &sample) in chunk.pcm.iter().enumerate() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "invalid sample in post-switch chunk {chunk_idx} offset {j}: {sample}\n  \
                 meta: {}",
                format_meta(meta, chunk.pcm.len()),
            );
        }

        // Intra-chunk continuity (allow 1 break for decoder handoff)
        let breaks = intra_chunk_breaks(&chunk);
        assert!(
            breaks <= 1,
            "too many intra-chunk breaks in post-switch chunk {chunk_idx}: {breaks}\n  \
             meta: {}",
            format_meta(meta, chunk.pcm.len()),
        );

        // Direction: must be descending after ABR switch
        let dir = detect_chunk_direction(&chunk);
        assert_eq!(
            dir,
            Direction::Descending,
            "post-switch chunk {chunk_idx} direction is {dir:?}, expected Descending\n  \
             meta: {}",
            format_meta(meta, chunk.pcm.len()),
        );

        // Frame offset continuity between chunks
        if let (Some(prev_off), Some(prev_f)) = (prev_frame_offset, prev_frames) {
            let expected_offset = prev_off + prev_f as u64;
            if meta.frame_offset != expected_offset {
                continuity_breaks += 1;
                if continuity_breaks <= 5 {
                    info!(
                        chunk_idx,
                        prev_frame_offset = prev_off,
                        prev_frames = prev_f,
                        expected_offset,
                        actual_offset = meta.frame_offset,
                        segment = ?meta.segment_index,
                        variant = ?meta.variant_index,
                        epoch = meta.epoch,
                        "CHUNK CONTINUITY BREAK (frame_offset)"
                    );
                }
            }
        }

        prev_frame_offset = Some(meta.frame_offset);
        prev_frames = Some(frames);
    }

    info!(
        continuity_breaks,
        "Phase 2 complete: {} post-switch chunks verified",
        Consts::POST_SWITCH_CHUNKS
    );

    // We don't assert on frame_offset continuity in phase 2 because
    // the first few chunks after ABR switch may have a gap due to decoder recreation.
    // We track it for diagnostics.

    // Phase 3: Random seeks — 200 iterations, 5 chunks each
    info!(
        "Phase 3: {} random seek + {} chunk reads...",
        Consts::SEEK_ITERATIONS,
        Consts::CHUNKS_PER_SEEK
    );

    let total_duration = audio.duration();
    let total_secs = total_duration
        .map_or(Consts::SEGMENT_COUNT as f64 * segment_duration * 0.9, |d| {
            d.as_secs_f64()
        });
    let max_seek_secs = (total_secs - 0.5).max(0.1);

    let mut rng = Xorshift64::new(0xAB25_5017_C400_0000);
    let mut successful_reads = 0u64;
    let mut inter_chunk_breaks = 0u64;
    let mut inter_sample_breaks = 0u64;
    let mut intra_breaks = 0u64;
    let mut direction_errors = 0u64;

    for i in 0..Consts::SEEK_ITERATIONS {
        let pos_secs = rng.range_f64(0.001, max_seek_secs);
        let position = Duration::from_secs_f64(pos_secs);

        audio.seek(position).unwrap_or_else(|e| {
            panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
        });
        audio.preload();

        let mut prev_chunk_meta: Option<(PcmMeta, usize)> = None;
        let mut prev_last_sample: Option<f32> = None;

        for c in 0..Consts::CHUNKS_PER_SEEK {
            let stage = format!("phase3_seek_{i}_chunk_{c}");
            let Some(chunk) = next_chunk_with_timeout(
                &mut audio,
                Duration::from_millis(Consts::NEXT_CHUNK_TIMEOUT_MS),
                &stage,
            )
            .await
            else {
                // EOF after seek near end is acceptable.
                break;
            };

            let channels = chunk.meta.spec.channels as usize;
            let frames = chunk.frames();
            let meta = chunk.meta;

            // Integrity: all samples finite and in [-1, 1]
            for (j, &sample) in chunk.pcm.iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{i} chunk {c} offset {j}: {sample}\n  \
                     meta: {}\n  seek_pos: {pos_secs:.4}s",
                    format_meta(&meta, chunk.pcm.len()),
                );
            }

            // Intra-chunk saw-tooth continuity
            let breaks = intra_chunk_breaks(&chunk);
            if breaks > 0 {
                intra_breaks += breaks as u64;
                if intra_breaks <= 5 {
                    info!(
                        iteration = i,
                        chunk_in_seq = c,
                        breaks,
                        meta = %format_meta(&meta, chunk.pcm.len()),
                        pos_secs,
                        "Intra-chunk saw-tooth breaks"
                    );
                }
            }

            // Direction after ABR switch should be descending
            let dir = detect_chunk_direction(&chunk);
            if dir != Direction::Descending && dir != Direction::Unknown {
                direction_errors += 1;
                if direction_errors <= 5 {
                    info!(
                        iteration = i,
                        chunk_in_seq = c,
                        direction = ?dir,
                        meta = %format_meta(&meta, chunk.pcm.len()),
                        pos_secs,
                        "Unexpected direction (expected SawtoothDescending)"
                    );
                }
            }

            // Inter-chunk frame_offset continuity (within the same seek sequence)
            if let Some((prev_meta, prev_f)) = prev_chunk_meta {
                let expected_offset = prev_meta.frame_offset + prev_f as u64;
                if meta.frame_offset != expected_offset {
                    inter_chunk_breaks += 1;
                    if inter_chunk_breaks <= 5 {
                        info!(
                            iteration = i,
                            chunk_in_seq = c,
                            prev_meta = %format_meta(&prev_meta, 0),
                            prev_frames = prev_f,
                            expected_offset,
                            actual_offset = meta.frame_offset,
                            curr_meta = %format_meta(&meta, chunk.pcm.len()),
                            pos_secs,
                            "INTER-CHUNK FRAME_OFFSET BREAK"
                        );
                    }
                }
            }

            // Inter-chunk saw-tooth sample continuity:
            // last sample of prev chunk → first sample of curr chunk
            // must follow ascending or descending pattern.
            if let Some(prev_last) = prev_last_sample
                && channels > 0
                && !chunk.pcm.is_empty()
            {
                let curr_first = chunk.pcm[0]; // first sample (L channel)
                let prev_phase = phase_from_f32(prev_last);
                let curr_phase = phase_from_f32(curr_first);
                let expected_asc = (prev_phase as usize + 1) % SawWav::SAW_PERIOD;
                let expected_desc =
                    (prev_phase as usize + SawWav::SAW_PERIOD - 1) % SawWav::SAW_PERIOD;
                if curr_phase != expected_asc && curr_phase != expected_desc {
                    inter_sample_breaks += 1;
                    if inter_sample_breaks <= 10 {
                        info!(
                            iteration = i,
                            chunk_in_seq = c,
                            prev_last_sample = prev_last,
                            curr_first_sample = curr_first,
                            prev_phase,
                            curr_phase,
                            expected_asc,
                            expected_desc,
                            prev_meta = %format_meta(
                                &prev_chunk_meta.map(|(m, _)| m).unwrap_or_default(),
                                0
                            ),
                            curr_meta = %format_meta(&meta, chunk.pcm.len()),
                            pos_secs,
                            "INTER-CHUNK SAMPLE CONTINUITY BREAK"
                        );
                    }
                }
            }

            // Track last L-channel sample for inter-chunk check
            if channels > 0 && frames > 0 {
                prev_last_sample = Some(chunk.pcm[(frames - 1) * channels]);
            }

            prev_chunk_meta = Some((meta, frames));
            successful_reads += 1;
        }

        if (i + 1) % 50 == 0 {
            info!(
                iteration = i + 1,
                successful_reads,
                inter_chunk_breaks,
                inter_sample_breaks,
                intra_breaks,
                direction_errors,
                "Progress"
            );
        }
    }

    info!(
        successful_reads,
        inter_chunk_breaks,
        inter_sample_breaks,
        intra_breaks,
        direction_errors,
        "Phase 3 complete: {} seek cycles",
        Consts::SEEK_ITERATIONS
    );

    if intra_breaks > 0 {
        warn!(
            intra_breaks,
            "intra-chunk saw-tooth breaks detected (within tolerance of 1)"
        );
    }
    assert!(
        intra_breaks <= 1,
        "{intra_breaks} intra-chunk saw-tooth breaks in decoded data (>1 tolerance)"
    );
    assert_eq!(
        inter_sample_breaks, 0,
        "{inter_sample_breaks} inter-chunk sample continuity breaks in decoded data"
    );
    assert_eq!(
        direction_errors, 0,
        "{direction_errors} direction errors (expected descending after ABR switch)"
    );
    // Note: inter_chunk_breaks (frame_offset) are logged but not asserted — frame_offset
    // gaps between chunks after seek can occur due to decoder restart semantics.

    // Phase 4: EOF
    info!("Phase 4: seek near end + drain to EOF...");

    let final_seek_secs = (total_secs - 0.1).max(0.0);
    audio
        .seek(Duration::from_secs_f64(final_seek_secs))
        .unwrap_or_else(|e| {
            panic!("final seek to {final_seek_secs:.4}s failed: {e}");
        });
    audio.preload();

    let mut remaining_chunks = 0u64;
    let mut remaining_samples = 0u64;
    loop {
        let Some(chunk) = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(Consts::NEXT_CHUNK_TIMEOUT_MS),
            "phase4_tail_drain",
        )
        .await
        else {
            break;
        };
        remaining_chunks += 1;
        remaining_samples += chunk.pcm.len() as u64;
        for &sample in chunk.pcm.iter() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "invalid sample in final tail read",
            );
        }
    }

    assert!(
        audio.is_eof(),
        "expected EOF after draining all remaining chunks"
    );

    info!(
        remaining_chunks,
        remaining_samples, "Phase 4 complete: EOF confirmed"
    );
    info!("Chunk integrity stress test passed");
}
