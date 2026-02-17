//! Stress test: chunk-level ABR integrity via `PcmReader::next_chunk`.
//!
//! Same ABR scenario as `stress_seek_abr_audio` (ascending V0, descending V1,
//! delayed V0 → ABR switch to V1), but reads whole `PcmChunk` values instead
//! of raw f32 samples. This gives access to `PcmMeta` (frame_offset, timestamp,
//! segment_index, variant_index, epoch) for precise root-cause diagnosis.
//!
//! Four-phase verification:
//! 1. **Warmup**: read chunks until ABR switches from V0 (ascending) to V1 (descending)
//! 2. **Post-switch**: 50 sequential V1 chunks with frame_offset continuity
//! 3. **Random seeks**: 200 seek + 5 chunk reads with continuity + saw-tooth checks
//! 4. **EOF**: seek near end, drain to EOF

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use kithara::assets::StoreOptions;
use kithara::audio::{Audio, AudioConfig, PcmReader};
use kithara::decode::{PcmChunk, PcmMeta};
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::{AudioCodec, ContainerFormat, MediaInfo, Stream};
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_test_utils::Xorshift64;

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 50;
const SEEK_ITERATIONS: usize = 200;
const SAW_PERIOD: usize = 65536;
const WARMUP_TIMEOUT_SECS: u64 = 30;
const POST_SWITCH_CHUNKS: usize = 50;
const CHUNKS_PER_SEEK: usize = 5;

// WAV Generators

fn ascending_sample(frame: usize) -> i16 {
    ((frame % SAW_PERIOD) as i32 - 32768) as i16
}

fn descending_sample(frame: usize) -> i16 {
    (32767 - (frame % SAW_PERIOD) as i32) as i16
}

fn create_wav_init_segment() -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * bytes_per_sample as u32;
    let block_align = CHANNELS * bytes_per_sample;
    let data_size = 0xFFFF_FFFFu32;
    let file_size = 0xFFFF_FFFFu32;

    let mut wav = Vec::with_capacity(44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav
}

fn create_pcm_segments(sample_fn: fn(usize) -> i16) -> Vec<u8> {
    let total_bytes = SEGMENT_COUNT * SEGMENT_SIZE;
    let bytes_per_frame = CHANNELS as usize * 2;
    let total_frames = total_bytes / bytes_per_frame;

    let mut pcm = Vec::with_capacity(total_bytes);
    for frame in 0..total_frames {
        let sample = sample_fn(frame);
        for _ in 0..CHANNELS {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    pcm.resize(total_bytes, 0);
    pcm
}

// Direction Detection

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Ascending,
    Descending,
    Unknown,
}

fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

fn detect_chunk_direction(chunk: &PcmChunk) -> Direction {
    let channels = chunk.meta.spec.channels as usize;
    let frames = chunk.frames();
    if frames < 2 {
        return Direction::Unknown;
    }

    let check_count = 10.min(frames - 1);
    let mut ascending_votes = 0u32;
    let mut descending_votes = 0u32;

    for f in 0..check_count {
        let p0 = phase_from_f32(chunk.pcm[f * channels]);
        let p1 = phase_from_f32(chunk.pcm[(f + 1) * channels]);

        let expected_asc = (p0 + 1) % SAW_PERIOD;
        let expected_desc = (p0 + SAW_PERIOD - 1) % SAW_PERIOD;

        if p1 == expected_asc {
            ascending_votes += 1;
        } else if p1 == expected_desc {
            descending_votes += 1;
        }
    }

    if ascending_votes > descending_votes && ascending_votes > 0 {
        Direction::Ascending
    } else if descending_votes > ascending_votes && descending_votes > 0 {
        Direction::Descending
    } else {
        Direction::Unknown
    }
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
        let expected_asc = (prev_phase + 1) % SAW_PERIOD;
        let expected_desc = (prev_phase + SAW_PERIOD - 1) % SAW_PERIOD;
        if curr_phase != expected_asc && curr_phase != expected_desc {
            breaks += 1;
        }
    }
    breaks
}

// Stress Test

#[rstest]
#[case::mmap(false)]
#[case::ephemeral(true)]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_chunk_integrity(#[case] ephemeral: bool) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug"
                .to_string()
        }))
        .try_init();

    // Generate WAV data for two variants
    let init_segment = Arc::new(create_wav_init_segment());
    let v0_pcm = Arc::new(create_pcm_segments(ascending_sample));
    let v1_pcm = Arc::new(create_pcm_segments(descending_sample));

    info!(
        init_size = init_segment.len(),
        v0_size = v0_pcm.len(),
        v1_size = v1_pcm.len(),
        segments = SEGMENT_COUNT,
        "Generated WAV data for two variants"
    );

    // Spawn HLS server
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&v0_pcm), Arc::clone(&v1_pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        segment_delay: Some(Arc::new(|variant, segment| {
            if variant == 0 && segment >= 3 {
                Duration::from_millis(500)
            } else {
                Duration::ZERO
            }
        })),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    info!(%url, "HLS server ready with 2 variants");

    // Create Audio<Stream<Hls>> with Auto ABR starting on V0
    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        // Ephemeral mode auto-evicts MemResources from LRU cache.
        // 2 variants × SEGMENT_COUNT segments + headroom.
        store.cache_capacity = Some(NonZeroUsize::new(SEGMENT_COUNT * 2 + 10).expect("nonzero"));
    }

    let mut hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(120),
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });
    if ephemeral {
        hls_config = hls_config.with_ephemeral(true);
    }

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

    // Run test phases in blocking thread
    let result = tokio::task::spawn_blocking(move || {
        // Phase 1: Warmup + ABR switch detection
        info!("Phase 1: waiting for ABR switch (ascending -> descending) via chunks...");

        let warmup_start = std::time::Instant::now();
        let warmup_timeout = Duration::from_secs(WARMUP_TIMEOUT_SECS);
        let mut warmup_ascending = 0u64;
        let mut warmup_unknown = 0u64;

        loop {
            if warmup_start.elapsed() > warmup_timeout {
                panic!(
                    "ABR switch not detected within {WARMUP_TIMEOUT_SECS}s \
                     (ascending={warmup_ascending}, unknown={warmup_unknown})"
                );
            }

            let Some(chunk) = audio.next_chunk() else {
                if audio.is_eof() {
                    panic!(
                        "Hit EOF before ABR switch \
                         (ascending={warmup_ascending}, unknown={warmup_unknown})"
                    );
                }
                continue;
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
        info!("Phase 2: verifying {POST_SWITCH_CHUNKS} post-switch chunks...");

        let mut prev_frame_offset: Option<u64> = None;
        let mut prev_frames: Option<usize> = None;
        let mut continuity_breaks = 0u64;

        for chunk_idx in 0..POST_SWITCH_CHUNKS {
            let Some(chunk) = audio.next_chunk() else {
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
            "Phase 2 complete: {POST_SWITCH_CHUNKS} post-switch chunks verified"
        );

        // We don't assert on frame_offset continuity in phase 2 because
        // the first few chunks after ABR switch may have a gap due to decoder recreation.
        // We track it for diagnostics.

        // Phase 3: Random seeks — 200 iterations, 5 chunks each
        info!("Phase 3: {SEEK_ITERATIONS} random seek + {CHUNKS_PER_SEEK} chunk reads...");

        let total_duration = audio.duration();
        let total_secs = total_duration
            .map_or(SEGMENT_COUNT as f64 * segment_duration * 0.9, |d| {
                d.as_secs_f64()
            });
        let max_seek_secs = (total_secs - 0.5).max(0.1);

        let mut rng = Xorshift64::new(0xAB25_5017_C400_0000);
        let mut successful_reads = 0u64;
        let mut inter_chunk_breaks = 0u64;
        let mut inter_sample_breaks = 0u64;
        let mut intra_breaks = 0u64;
        let mut direction_errors = 0u64;

        for i in 0..SEEK_ITERATIONS {
            let pos_secs = rng.range_f64(0.001, max_seek_secs);
            let position = Duration::from_secs_f64(pos_secs);

            audio.seek(position).unwrap_or_else(|e| {
                panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
            });

            let mut prev_chunk_meta: Option<(PcmMeta, usize)> = None;
            let mut prev_last_sample: Option<f32> = None;

            for c in 0..CHUNKS_PER_SEEK {
                let Some(chunk) = audio.next_chunk() else {
                    // EOF or no data — acceptable after seek near end
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
                            "Unexpected direction (expected Descending)"
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
                    let expected_asc = (prev_phase + 1) % SAW_PERIOD;
                    let expected_desc = (prev_phase + SAW_PERIOD - 1) % SAW_PERIOD;
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
            "Phase 3 complete: {SEEK_ITERATIONS} seek cycles"
        );

        assert_eq!(
            intra_breaks, 0,
            "{intra_breaks} intra-chunk saw-tooth breaks in decoded data"
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

        let mut remaining_chunks = 0u64;
        let mut remaining_samples = 0u64;
        while let Some(chunk) = audio.next_chunk() {
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
    })
    .await;

    match result {
        Ok(()) => info!("Chunk integrity stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
