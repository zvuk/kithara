use std::num::{NonZeroU128, NonZeroUsize};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ChunkOutcome},
    hls::{Hls, HlsConfig},
    platform::{
        CancelToken,
        sync::Arc,
        thread,
        time::{Duration, WallInstant},
    },
    play::{PlayWorker, PlayWorkerConfig},
    signal::{AudioChunk, AudioChunkInfo},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper, auto,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::DelayRule,
    mock::LaneAudio,
    reads::blocking_audio,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_fixtures::hls_fixtures::{
    hls_header_fifty, hls_pcm_fifty, hls_pcm_fifty_descending,
};
use kithara_test_fixtures::signal::{self, SignalDirection as Direction, detect_direction};
use kithara_test_utils::{TestTempDir, Xorshift64};
use num_traits::AsPrimitive;
use tracing::info;

use crate::{
    common::test_defaults::SawWav,
    saw_chunk::{
        channel_mismatches, check_fractional_saw, check_fractional_saw_controls,
        first_invalid_sample,
    },
};

mod consts {
    use super::SawWav;

    pub(super) const D: SawWav = SawWav::DEFAULT;
    pub(super) const SEGMENT_COUNT: usize = 50;
    pub(super) const SEEK_ITERATIONS: usize = 200;
    pub(super) const WARMUP_TIMEOUT_SECS: u64 = 30;
    pub(super) const POST_SWITCH_CHUNKS: usize = 50;
    pub(super) const CHUNKS_PER_SEEK: usize = 5;
    pub(super) const WARMUP_NEXT_CHUNK_TIMEOUT_MS: u64 = 5_000;
    pub(super) const NEXT_CHUNK_TIMEOUT_MS: u64 = 3_000;
}

fn detect_chunk_direction(chunk: &AudioChunk) -> Direction {
    let channels = chunk.meta.spec.channels as usize;
    detect_direction(&chunk.samples, channels)
}

/// Format chunk metadata for diagnostic output.
fn format_meta(meta: &AudioChunkInfo, pcm_len: usize) -> String {
    format!(
        "frame_offset={}, samples={}, segment={:?}, variant={:?}, epoch={}",
        meta.frame_offset,
        pcm_len,
        meta.segment_index,
        meta.variant_index,
        meta.segment.get()
    )
}

/// Check saw-tooth continuity within a single chunk.
/// Returns the number of breaks found.
fn intra_chunk_breaks(chunk: &AudioChunk) -> usize {
    let channels = chunk.meta.spec.channels as usize;
    let frames = chunk.frames();
    if frames < 2 {
        return 0;
    }

    let mut breaks = 0;
    for f in 1..frames {
        let prev_phase = signal::phase::units(chunk.samples[(f - 1) * channels]);
        let curr_phase = signal::phase::units(chunk.samples[f * channels]);
        let expected_asc = (prev_phase + 1) % signal::SAW_PERIOD;
        let expected_desc = (prev_phase + signal::SAW_PERIOD - 1) % signal::SAW_PERIOD;
        if curr_phase != expected_asc && curr_phase != expected_desc {
            breaks += 1;
        }
    }
    breaks
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn next_chunk_with_timeout<R: AudioRead>(
    audio: &mut R,
    timeout: Duration,
    stage: &str,
) -> Option<AudioChunk> {
    let deadline = WallInstant::now() + timeout;
    loop {
        hang_tick!();
        match AudioRead::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                hang_reset!();
                return Some(*chunk);
            }
            Ok(ChunkOutcome::Eof { .. }) => {
                hang_reset!();
                return None;
            }
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("next_chunk decode error at stage='{stage}': {e}"),
        }
        assert!(
            WallInstant::now() <= deadline,
            "next_chunk timeout at stage='{stage}'"
        );
        thread::paced_backoff(Duration::from_micros(500));
    }
}

#[kithara::fixture]
async fn audio_server(
    hls_header_fifty: Vec<u8>,
    hls_pcm_fifty: Vec<u8>,
    hls_pcm_fifty_descending: Vec<u8>,
) -> CreatedHls {
    let init_segment = Arc::new(hls_header_fifty);
    let v0_pcm = Arc::new(hls_pcm_fifty);
    let v1_pcm = Arc::new(hls_pcm_fifty_descending);

    info!(
        init_size = init_segment.len(),
        v0_size = v0_pcm.len(),
        v1_size = v1_pcm.len(),
        segments = consts::SEGMENT_COUNT,
        "Generated WAV data for two variants"
    );

    let segment_duration = consts::D.segment_duration_secs();

    TestServerHelper::new()
        .await
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(2)
                .segments_per_variant(consts::SEGMENT_COUNT)
                .segment_size(consts::D.segment_size)
                .segment_duration_secs(segment_duration)
                .custom_data_per_variant(vec![Arc::clone(&v0_pcm), Arc::clone(&v1_pcm)])
                .init_data_per_variant(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)])
                .variant_bandwidths(vec![5_000_000, 1_000_000])
                .delay_rules(vec![DelayRule {
                    variant: Some(0),
                    segment_gte: Some(3),
                    delay_ms: 500,
                    ..Default::default()
                }]),
        )
        .await
        .expect("create HLS fixture")
}

#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
)]
#[case::mmap(false)]
#[case::ephemeral(true)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn stress_chunk_integrity(#[future(awt)] audio_server: CreatedHls, #[case] ephemeral: bool) {
    let server = audio_server;
    let segment_duration = server.spec().segment_duration_secs;
    let url = server.master_url();
    info!(%url, "HLS server ready with 2 variants");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let store = if ephemeral {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cache_capacity(NonZeroUsize::new(consts::SEGMENT_COUNT * 2 + 10).expect("nonzero"))
            .build()
    } else {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().to_path_buf(),
            })
            .build()
    };

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools)
        .cancel(cancel)
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .build();
    let audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    let _ = blocking_audio(audio, move |audio| {
        check_chunk_integrity(audio, segment_duration);
    })
    .await;
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn check_chunk_integrity(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    segment_duration: f64,
) {
    check_fractional_saw_controls();
    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    audio.preload().expect("preload must succeed");

    info!("Phase 1: waiting for ABR switch (ascending -> descending) via chunks...");

    let warmup_start = WallInstant::now();
    let warmup_timeout = Duration::from_secs(consts::WARMUP_TIMEOUT_SECS);
    let mut warmup_ascending = 0u64;
    let mut warmup_unknown = 0u64;

    loop {
        hang_tick!();
        if warmup_start.elapsed() > warmup_timeout {
            panic!(
                "ABR switch not detected within {}s (ascending={}, unknown={})",
                consts::WARMUP_TIMEOUT_SECS,
                warmup_ascending,
                warmup_unknown
            );
        }

        let Some(chunk) = next_chunk_with_timeout(
            audio,
            Duration::from_millis(consts::WARMUP_NEXT_CHUNK_TIMEOUT_MS),
            "phase1_warmup",
        ) else {
            panic!(
                "Hit EOF before ABR switch (ascending={}, unknown={})",
                warmup_ascending, warmup_unknown
            );
        };

        hang_reset!();
        let dir = detect_chunk_direction(&chunk);
        match dir {
            Direction::Ascending => {
                warmup_ascending += 1;
            }
            Direction::Descending if intra_chunk_breaks(&chunk) <= 1 => {
                info!(
                    warmup_ascending,
                    warmup_unknown,
                    elapsed_ms = warmup_start.elapsed().as_millis(),
                    chunk_meta = %format_meta(&chunk.meta, chunk.samples.len()),
                    "ABR switch detected: ascending -> descending"
                );
                break;
            }
            Direction::Descending | Direction::Unknown => {
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

    info!(
        "Phase 2: verifying {} post-switch chunks...",
        consts::POST_SWITCH_CHUNKS
    );

    let mut prev_frame_offset: Option<u64> = None;
    let mut prev_frames: Option<usize> = None;
    let mut continuity_breaks = 0u64;

    for chunk_idx in 0..consts::POST_SWITCH_CHUNKS {
        hang_tick!();
        let stage = format!("phase2_post_switch_chunk_{chunk_idx}");
        let Some(chunk) = next_chunk_with_timeout(
            audio,
            Duration::from_millis(consts::NEXT_CHUNK_TIMEOUT_MS),
            &stage,
        ) else {
            panic!("next_chunk returned None at post-switch chunk {chunk_idx}",);
        };
        hang_reset!();

        let frames = chunk.frames();
        let meta = &chunk.meta;

        if let Some((j, sample)) = first_invalid_sample(&chunk.samples) {
            panic!(
                "invalid sample in post-switch chunk {chunk_idx} offset {j}: {sample}\n  \
                 meta: {}",
                format_meta(meta, chunk.samples.len()),
            );
        }
        if meta.spec.channels == 2
            && let Some((frame, left, right)) = channel_mismatches(&chunk.samples).next()
        {
            panic!(
                "L/R mismatch in post-switch chunk {chunk_idx}, frame {frame}: left={left}, right={right}; meta={meta:?}"
            );
        }

        let breaks = intra_chunk_breaks(&chunk);
        assert!(
            breaks <= 1,
            "too many intra-chunk breaks in post-switch chunk {chunk_idx}: {breaks}\n  \
             meta: {}",
            format_meta(meta, chunk.samples.len()),
        );

        let dir = detect_chunk_direction(&chunk);
        assert_eq!(
            dir,
            Direction::Descending,
            "post-switch chunk {chunk_idx} direction is {dir:?}, expected Descending\n  \
             meta: {}",
            format_meta(meta, chunk.samples.len()),
        );

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
                        epoch = meta.segment.get(),
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
        consts::POST_SWITCH_CHUNKS
    );

    info!(
        "Phase 3: {} random seek + {} chunk reads...",
        consts::SEEK_ITERATIONS,
        consts::CHUNKS_PER_SEEK
    );

    let total_duration = audio.duration();
    let segments: f64 = consts::SEGMENT_COUNT.as_();
    let total_secs = total_duration.map_or(segments * segment_duration * 0.9, |d| d.as_secs_f64());
    let max_seek_secs = (total_secs - 0.5).max(0.1);

    let mut rng = Xorshift64::new(0xAB25_5017_C400_0000);
    let mut successful_reads = 0u64;
    let mut inter_chunk_breaks = 0u64;
    let mut inter_sample_breaks = 0u64;
    let mut intra_breaks = 0u64;
    let mut first_intra_error = None;
    let mut direction_errors = 0u64;

    for i in 0..consts::SEEK_ITERATIONS {
        hang_tick!();
        let pos_secs = rng.range_f64(0.001, max_seek_secs);
        let position = Duration::from_secs_f64(pos_secs);

        audio.seek(position).unwrap_or_else(|e| {
            panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
        });
        audio.preload().expect("preload must succeed");

        let mut prev_chunk_meta: Option<(AudioChunkInfo, usize)> = None;
        let mut previous_raw_end: Option<(u128, NonZeroU128)> = None;

        for c in 0..consts::CHUNKS_PER_SEEK {
            hang_tick!();
            let stage = format!("phase3_seek_{i}_chunk_{c}");
            let Some(chunk) = next_chunk_with_timeout(
                audio,
                Duration::from_millis(consts::NEXT_CHUNK_TIMEOUT_MS),
                &stage,
            ) else {
                break;
            };
            hang_reset!();

            let channels = chunk.meta.spec.channels as usize;
            let frames = chunk.frames();
            let meta = chunk.meta;

            for (j, &sample) in chunk.samples.iter().enumerate() {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "invalid sample at seek #{i} chunk {c} offset {j}: {sample}\n  \
                     meta: {}\n  seek_pos: {pos_secs:.4}s",
                    format_meta(&meta, chunk.samples.len()),
                );
            }

            let span = meta
                .source_span
                .expect("post-seek PCM has an exact source span");
            if let Err(error) = check_fractional_saw(
                &chunk.samples,
                channels,
                span,
                signal::Wave::SawtoothDescending,
                (c == 0).then_some(position),
            ) {
                intra_breaks += 1;
                first_intra_error.get_or_insert_with(|| {
                    format!("seek {i}, chunk {c}, requested={position:?}, meta={meta:?}: {error}")
                });
            }

            let dir = detect_chunk_direction(&chunk);
            if dir != Direction::Descending && dir != Direction::Unknown {
                direction_errors += 1;
                if direction_errors <= 5 {
                    info!(
                        iteration = i,
                        chunk_in_seq = c,
                        direction = ?dir,
                        meta = %format_meta(&meta, chunk.samples.len()),
                        pos_secs,
                        "Unexpected direction (expected SawtoothDescending)"
                    );
                }
            }

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
                            curr_meta = %format_meta(&meta, chunk.samples.len()),
                            pos_secs,
                            "INTER-CHUNK FRAME_OFFSET BREAK"
                        );
                    }
                }
            }

            let raw_start = span.source_ratio_at(0).expect("post-seek raw origin");
            if let Some((previous_numerator, previous_denominator)) = previous_raw_end {
                let previous_cross = previous_numerator
                    .checked_mul(raw_start.1.get())
                    .expect("previous raw endpoint cross product");
                let current_cross = raw_start
                    .0
                    .checked_mul(previous_denominator.get())
                    .expect("current raw origin cross product");
                if previous_cross != current_cross {
                    inter_sample_breaks += 1;
                }
            }
            previous_raw_end = span.source_ratio_at(frames as u64);

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
        consts::SEEK_ITERATIONS
    );

    assert_eq!(
        intra_breaks, 0,
        "fractional saw content or geometry differs: {first_intra_error:?}"
    );
    assert_eq!(
        inter_chunk_breaks, 0,
        "post-seek physical chunk frame labels must be adjacent",
    );
    assert_eq!(
        inter_sample_breaks, 0,
        "{inter_sample_breaks} inter-chunk sample continuity breaks in decoded data"
    );
    assert_eq!(
        direction_errors, 0,
        "{direction_errors} direction errors (expected descending after ABR switch)"
    );

    info!("Phase 4: seek near end + drain to EOF...");

    let final_seek_secs = (total_secs - 0.1).max(0.0);
    audio
        .seek(Duration::from_secs_f64(final_seek_secs))
        .unwrap_or_else(|e| {
            panic!("final seek to {final_seek_secs:.4}s failed: {e}");
        });
    audio
        .preload()
        .expect("preload after final seek must succeed");

    let mut remaining_chunks = 0u64;
    let mut remaining_samples = 0u64;
    hang_reset!();
    loop {
        hang_tick!();
        let Some(chunk) = next_chunk_with_timeout(
            audio,
            Duration::from_millis(consts::NEXT_CHUNK_TIMEOUT_MS),
            "phase4_tail_drain",
        ) else {
            break;
        };
        remaining_chunks += 1;
        hang_reset!();
        remaining_samples += chunk.samples.len() as u64;
        for &sample in chunk.samples.iter() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "invalid sample in final tail read",
            );
        }
    }

    info!(
        remaining_chunks,
        remaining_samples, "Phase 4 complete: EOF confirmed"
    );
    info!("Chunk integrity stress test passed");
}
