use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    hls::{Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper, auto,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::{DelayRule, PcmPattern},
    mock::LaneAudio,
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
    saw_chunk::{channel_mismatches, first_invalid_sample, phase_steps},
};

mod consts {
    use super::SawWav;

    pub(super) const D: SawWav = SawWav::DEFAULT;
    pub(super) const VARIANT_COUNT: usize = 2;
    pub(super) const SEGMENT_COUNT: usize = 50;
    pub(super) const SEEK_ITERATIONS: usize = 200;
    pub(super) const WARMUP_TIMEOUT_SECS: u64 = 30;
    /// Length of one read, in seconds of audio.
    pub(super) const CHUNK_SECS: f64 = 0.05;
    /// Chunks right after the switch that must all be descending.
    pub(super) const POST_SWITCH_CHUNKS: usize = 10;
}

#[derive(Clone, Copy, Debug)]
enum AbrAudioFixture {
    WavFileLike,
    FlacFmp4,
}

impl AbrAudioFixture {
    const fn media_info(self) -> MediaInfo {
        match self {
            Self::WavFileLike => MediaInfo::builder()
                .maybe_codec(Some(AudioCodec::Pcm))
                .maybe_container(Some(ContainerFormat::Wav))
                .build(),
            Self::FlacFmp4 => MediaInfo::builder()
                .maybe_codec(Some(AudioCodec::Flac))
                .maybe_container(Some(ContainerFormat::Fmp4))
                .build(),
        }
    }
}

fn assert_abr_size_probes(fixture: AbrAudioFixture, helper: &TestServerHelper, hls: &CreatedHls) {
    let total = helper.total_size_probe_count(hls);
    let per_variant: Vec<u64> = (0..hls.spec().variant_count)
        .map(|variant| helper.variant_size_probe_count(hls, variant))
        .collect();
    info!(
        ?fixture,
        total,
        ?per_variant,
        "size-probe counts after ABR stress"
    );
    match fixture {
        AbrAudioFixture::WavFileLike => {
            let all_variant_bound =
                u64::try_from(hls.spec().variant_count * hls.spec().segments_per_variant)
                    .expect("small fixture size");
            assert!(
                total < all_variant_bound,
                "WAV ABR lazy size probes must not probe every segment of every variant: \
                 total={total}, all_variant_bound={all_variant_bound}, per_variant={per_variant:?}"
            );
        }
        AbrAudioFixture::FlacFmp4 => {
            assert_eq!(
                total, 0,
                "FLAC/fMP4 ABR seeks must not issue size-probes; per_variant={per_variant:?}"
            );
        }
    }
}

type HlsAudio = LaneAudio<Stream<Hls<TestPools>>, TestPools>;

/// Phase 1: read until the saw turns from ascending to descending, the ABR
/// switch onto the low-bandwidth variant.
fn wait_for_abr_switch(audio: &mut HlsAudio, buf: &mut [f32], channels: usize) {
    info!("Phase 1: waiting for ABR switch (ascending -> descending)...");

    let warmup_start = kithara::platform::time::Instant::now();
    let warmup_timeout = Duration::from_secs(consts::WARMUP_TIMEOUT_SECS);
    let mut warmup_ascending_chunks = 0u64;
    let mut warmup_unknown_chunks = 0u64;

    loop {
        assert!(
            warmup_start.elapsed() <= warmup_timeout,
            "ABR switch not detected within {}s (ascending={warmup_ascending_chunks}, \
             unknown={warmup_unknown_chunks})",
            consts::WARMUP_TIMEOUT_SECS,
        );

        let n = match audio.read(buf) {
            Ok(ReadOutcome::Pending { .. }) => continue,
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Eof { .. }) => panic!(
                "Hit EOF before ABR switch (ascending={warmup_ascending_chunks}, \
                 unknown={warmup_unknown_chunks})"
            ),
            Err(e) => panic!("warmup decode error: {e}"),
        };

        match detect_direction(&buf[..n], channels) {
            Direction::Ascending => warmup_ascending_chunks += 1,
            Direction::Descending => {
                info!(
                    warmup_ascending_chunks,
                    warmup_unknown_chunks,
                    elapsed_ms = warmup_start.elapsed().as_millis(),
                    "ABR switch detected: ascending -> descending"
                );
                return;
            }
            Direction::Unknown => warmup_unknown_chunks += 1,
        }

        if warmup_ascending_chunks.is_multiple_of(100) && warmup_ascending_chunks > 0 {
            info!(
                warmup_ascending_chunks,
                warmup_unknown_chunks,
                elapsed_ms = warmup_start.elapsed().as_millis(),
                "Still waiting for ABR switch..."
            );
        }
    }
}

/// Phase 2: every chunk right after the switch is valid, descending saw, with
/// at most the one break the decoder handoff leaves.
fn verify_post_switch_chunks(audio: &mut HlsAudio, buf: &mut [f32], channels: usize) {
    info!(
        "Phase 2: verifying {} post-switch chunks are descending...",
        consts::POST_SWITCH_CHUNKS
    );

    for chunk_idx in 0..consts::POST_SWITCH_CHUNKS {
        let n = match audio.read(buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Pending { .. }) => {
                panic!("read returned 0 in post-switch chunk {chunk_idx}");
            }
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF in post-switch chunk {chunk_idx}");
            }
            Err(e) => panic!("post-switch decode error at chunk {chunk_idx}: {e}"),
        };
        let chunk = &buf[..n];

        if let Some((j, sample)) = first_invalid_sample(chunk) {
            panic!("invalid sample in post-switch chunk {chunk_idx} offset {j}: {sample}");
        }
        if channels == 2
            && let Some((f, l, r)) = channel_mismatches(chunk).next()
        {
            panic!("L/R mismatch in post-switch chunk {chunk_idx} frame {f}: L={l} R={r}");
        }

        let break_count = phase_steps(chunk, channels)
            .into_iter()
            .filter(|&(_, prev, curr)| signal::phase::delta(prev, curr) != -1)
            .count();
        assert!(
            break_count <= 1,
            "too many continuity breaks in post-switch chunk {chunk_idx}: {break_count}"
        );
        if break_count == 1 {
            info!(
                chunk_idx,
                "post-switch chunk has 1 expected decoder handoff break"
            );
        }

        let dir = detect_direction(chunk, channels);
        assert_eq!(
            dir,
            Direction::Descending,
            "post-switch chunk {chunk_idx} direction is {dir:?}, expected Descending"
        );
    }

    info!(
        post_switch_ok = consts::POST_SWITCH_CHUNKS,
        "Phase 2 complete: all post-switch chunks descending"
    );
}

/// What the random seek cycles found wrong, out of how many reads.
#[derive(Debug, Default)]
struct SeekTally {
    successful_reads: u64,
    channel_mismatches: u64,
    continuity_errors: u64,
    direction_errors: u64,
}

impl SeekTally {
    /// Tally one post-seek chunk: valid samples, channels that agree, a saw
    /// that steps one frame at a time, and no ascending run after the switch.
    fn record(&mut self, iteration: usize, pos_secs: f64, chunk: &[f32], channels: usize) {
        if let Some((j, sample)) = first_invalid_sample(chunk) {
            panic!("invalid sample at seek #{iteration} offset {j}: {sample} (pos {pos_secs:.4}s)");
        }

        if channels == 2 {
            for (frame, l, r) in channel_mismatches(chunk) {
                self.channel_mismatches += 1;
                if self.channel_mismatches <= 3 {
                    info!(iteration, frame, l, r, pos_secs, "Channel mismatch");
                }
            }
        }

        for (frame, prev_phase, curr_phase) in phase_steps(chunk, channels) {
            if signal::phase::distance(prev_phase, curr_phase) != 1 {
                self.continuity_errors += 1;
                if self.continuity_errors <= 3 {
                    info!(
                        iteration,
                        frame, prev_phase, curr_phase, pos_secs, "Continuity break"
                    );
                }
            }
        }

        let dir = detect_direction(chunk, channels);
        if dir != Direction::Descending && dir != Direction::Unknown {
            self.direction_errors += 1;
            if self.direction_errors <= 3 {
                info!(
                    iteration,
                    direction = ?dir,
                    pos_secs,
                    "Unexpected direction (expected SawtoothDescending)"
                );
            }
        }

        self.successful_reads += 1;
    }
}

/// Phase 3: random seeks each read back a clean descending chunk.
fn random_seek_cycles(audio: &mut HlsAudio, buf: &mut [f32], channels: usize, max_seek_secs: f64) {
    info!(
        "Phase 3: {} random seek+read cycles...",
        consts::SEEK_ITERATIONS
    );

    let mut rng = Xorshift64::new(0xAB25_5017_C400_0000);
    let mut tally = SeekTally::default();

    for i in 0..consts::SEEK_ITERATIONS {
        let pos_secs = rng.range_f64(0.001, max_seek_secs);
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|e| panic!("seek #{i} to {pos_secs:.4}s failed: {e}"));

        let n = match audio.read(buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Pending { .. } | ReadOutcome::Eof { .. }) => continue,
            Err(e) => panic!("seek read error at iteration {i}: {e}"),
        };
        tally.record(i, pos_secs, &buf[..n], channels);

        if (i + 1) % 50 == 0 {
            info!(iteration = i + 1, ?tally, "Progress");
        }
    }

    info!(
        ?tally,
        "Phase 3 complete: {} seek+read cycles",
        consts::SEEK_ITERATIONS
    );

    assert_eq!(
        tally.channel_mismatches, 0,
        "L/R channel data diverged {} times",
        tally.channel_mismatches
    );
    assert_eq!(
        tally.continuity_errors, 0,
        "{} continuity breaks in decoded data",
        tally.continuity_errors
    );
    assert_eq!(
        tally.direction_errors, 0,
        "{} direction errors (expected descending after ABR switch)",
        tally.direction_errors
    );
}

/// Phase 4: a seek near the end reads valid samples through to EOF.
fn drain_tail_to_eof(audio: &mut HlsAudio, buf: &mut [f32], final_seek_secs: f64) {
    info!("Phase 4: seek near end + read to EOF...");

    audio
        .seek(Duration::from_secs_f64(final_seek_secs))
        .unwrap_or_else(|e| panic!("final seek to {final_seek_secs:.4}s failed: {e}"));

    let mut remaining_samples = 0u64;
    let mut saw_eof = false;
    loop {
        match audio.read(buf) {
            Ok(ReadOutcome::Pending { .. }) => break,
            Ok(ReadOutcome::Frames { count, .. }) => {
                remaining_samples += count.get() as u64;
                assert!(
                    first_invalid_sample(&buf[..count.get()]).is_none(),
                    "invalid sample in final tail read",
                );
            }
            Ok(ReadOutcome::Eof { .. }) => {
                saw_eof = true;
                break;
            }
            Err(e) => panic!("final drain error: {e}"),
        }
    }

    assert!(saw_eof, "expected EOF after reading all remaining data");

    info!(remaining_samples, "Phase 4 complete: EOF confirmed");
}

#[kithara::fixture]
async fn wav_abr(
    hls_header_fifty: Vec<u8>,
    hls_pcm_fifty: Vec<u8>,
    hls_pcm_fifty_descending: Vec<u8>,
) -> (TestServerHelper, CreatedHls) {
    let segment_duration = consts::D.segment_duration_secs();
    let delay_rules = vec![DelayRule {
        variant: Some(0),
        segment_gte: Some(3),
        delay_ms: 500,
        ..Default::default()
    }];
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

    let helper = TestServerHelper::new().await;
    let server = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(consts::VARIANT_COUNT)
                .segments_per_variant(consts::SEGMENT_COUNT)
                .segment_size(consts::D.segment_size)
                .segment_duration_secs(segment_duration)
                .custom_data_per_variant(vec![Arc::clone(&v0_pcm), Arc::clone(&v1_pcm)])
                .init_data_per_variant(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)])
                .variant_bandwidths(vec![5_000_000, 1_000_000])
                .delay_rules(delay_rules.clone()),
        )
        .await
        .expect("create HLS fixture");
    (helper, server)
}

#[kithara::fixture]
async fn flac_abr() -> (TestServerHelper, CreatedHls) {
    let segment_duration = consts::D.segment_duration_secs();
    let delay_rules = vec![DelayRule {
        variant: Some(0),
        segment_gte: Some(3),
        delay_ms: 500,
        ..Default::default()
    }];
    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(consts::VARIANT_COUNT)
                .segments_per_variant(consts::SEGMENT_COUNT)
                .segment_duration_secs(segment_duration)
                .variant_bandwidths(vec![5_000_000, 1_000_000])
                .delay_rules(delay_rules)
                .packaged_audio_per_variant_pcm_flac(
                    consts::D.sample_rate,
                    consts::D.channels,
                    vec![PcmPattern::Ascending, PcmPattern::Descending],
                ),
        )
        .await
        .expect("create FLAC/fMP4 ABR fixture");
    (helper, created)
}

/// ABR variant switch stress test with ascending/descending saw-tooth verification.
///
/// Scenario:
/// 1. Two variants: V0 (ascending, high bandwidth, delayed) and V1 (descending, low bandwidth)
/// 2. ABR starts on V0, switches to V1 when V0 segments become slow
/// 3. Verify the switch happened via PCM direction change
/// 4. 200 random seeks with direction + integrity checks
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(3),
    tracing("kithara_abr=debug,kithara_audio=debug,kithara_hls=debug,kithara_stream=debug")
)]
#[case::wav_file_like(AbrAudioFixture::WavFileLike, wav_abr().await)]
#[case::flac_fmp4(AbrAudioFixture::FlacFmp4, flac_abr().await)]
async fn stress_seek_abr_audio(
    #[case] fixture: AbrAudioFixture,
    #[case] prepared: (TestServerHelper, CreatedHls),
) {
    let (helper, hls) = prepared;
    let url = hls.master_url();

    info!(?fixture, %url, "HLS server ready with 2 variants");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .cancel(cancel)
        .initial_abr_mode(auto(0))
        .build();

    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .media_info(fixture.media_info())
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio pipeline created"
    );

    let result = spawn_blocking(move || {
        let _ = audio.preload();
        let channels = usize::from(spec.channels);
        let chunk_samples = num_traits::cast::<f64, usize>(
            consts::CHUNK_SECS * f64::from(spec.sample_rate.get()) * f64::from(spec.channels),
        )
        .unwrap_or(usize::MAX);
        let mut buf = vec![0.0f32; chunk_samples];

        wait_for_abr_switch(&mut audio, &mut buf, channels);
        verify_post_switch_chunks(&mut audio, &mut buf, channels);

        let segments: f64 = consts::SEGMENT_COUNT.as_();
        let total_secs = audio
            .duration()
            .map_or(segments * consts::CHUNK_SECS * 20.0, |d| d.as_secs_f64());
        random_seek_cycles(
            &mut audio,
            &mut buf,
            channels,
            (total_secs - consts::CHUNK_SECS).max(0.1),
        );
        drain_tail_to_eof(
            &mut audio,
            &mut buf,
            (total_secs - consts::CHUNK_SECS).max(0.0),
        );
    })
    .await;

    match result {
        Ok(()) => {
            assert_abr_size_probes(fixture, &helper, &hls);
            info!(?fixture, "ABR stress test passed");
        }
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
