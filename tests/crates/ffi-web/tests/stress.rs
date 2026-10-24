use std::num::NonZeroUsize;

use gloo_timers::future::TimeoutFuture;
use js_sys::{Date, Promise};
use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{
        Audio, AudioConfig, AudioControl, AudioEvent, AudioRead, AudioSession, AudioSource,
        ChunkOutcome, DecoderEvent, ReadOutcome, SeekLifecycleStage, SourceEnd,
    },
    events::EventBus,
    hls::{Hls, HlsConfig},
    platform::time::{Duration, Instant},
    play::{PlayWorker, PlayWorkerConfig},
    signal::SourceSpan,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, auto,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::DataMode,
    hls_fixture::create_test_downloader,
    mock::{LaneAudio, wait_for_preload},
};
use kithara_test_fixtures::signal;
use kithara_test_utils::Xorshift64;
use num_traits::ToPrimitive;
use tracing::{info, warn};
use url::Url;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::MessageChannel;

async fn create_stress_source(jitter: bool) -> (TestServerHelper, Url) {
    kithara::platform::logging::install_panic_hook();
    let _ = tracing_wasm::try_set_as_global_default();
    let helper = TestServerHelper::new().await;
    let bytes_per_second = 44_100.0 * 2.0 * 2.0;
    let base_builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(48)
        .segment_size(200_000)
        .segment_duration_secs(200_000.0 / bytes_per_second)
        // `SawWav` is a whole WAV file cut into segments, so segment 0 already
        // carries the 44-byte header. An `EXT-X-MAP` header on top of it is a
        // second copy: the reader parses the init one and the segment's own
        // lands in the data chunk, where 44 bytes read back as 11 frames of
        // noise. The two are alternative fixture shapes, not layers.
        .data_mode(DataMode::SawWav {
            sample_rate: 44_100,
            channels: 2,
        });
    let builder = if jitter {
        base_builder
            .segment_size(180_000)
            .segment_duration_secs(180_000.0 / bytes_per_second)
    } else {
        base_builder
    };
    let fixture = helper
        .create_hls(builder)
        .await
        .expect("create WASM stress fixture");
    let url = fixture.master_url();
    (helper, url)
}

/// Every segment of the stress fixture plus its init resource, so a random
/// seek reads what an earlier one downloaded.
const STRESS_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(64).unwrap();

fn stress_pipeline_config(url: Url) -> (PlayWorker<TestPools>, AudioConfig<Hls<TestPools>>) {
    const EVENT_BUS_CAPACITY: usize = 4096;
    let bus = EventBus::new(EVENT_BUS_CAPACITY);
    let pools = pools();

    let hls_config = HlsConfig::for_url(url)
        .downloader(create_test_downloader())
        .events(bus)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                // The store's default in-memory capacity holds an init
                // segment and two or three media segments, which is what
                // sequential playback needs. These tests seek at random
                // across all 48 segments of the fixture, so that default
                // turns almost every seek into a fresh network fetch of the
                // segment it lands in and the suite measures the server
                // rather than the pipeline. The capacity covers the whole
                // fixture instead.
                .cache_capacity(STRESS_CACHE_CAPACITY)
                .build(),
        )
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    (worker, config)
}

async fn create_pipeline_with_url(url: Url) -> LaneAudio<Stream<Hls<TestPools>>, TestPools> {
    let (worker, config) = stress_pipeline_config(url);
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .unwrap();
    audio
        .preload()
        .expect("start preloading the stress fixture");
    audio
}

#[kithara_test_utils::kithara::hang_watchdog]
async fn read_committed_source(audio: &mut Audio<Stream<Hls<TestPools>>>) -> Option<usize> {
    loop {
        hang_tick!();
        match audio.next_chunk().expect("read source chunk") {
            ChunkOutcome::Chunk(chunk) => {
                let end = chunk.meta.source_span.map_or_else(
                    || {
                        SourceEnd::new(
                            chunk.meta.frame_offset + u64::from(chunk.meta.frames),
                            chunk.meta.spec.sample_rate,
                        )
                        .with_mapping_revision(chunk.meta.mapping_revision)
                    },
                    |span| {
                        SourceEnd::new(span.end(), span.sample_rate())
                            .with_mapping_revision(span.mapping_revision())
                    },
                );
                audio.commit_source_end(end, chunk.meta);
                audio.finish_deferred();
                hang_reset!();
                return Some(chunk.samples.len());
            }
            ChunkOutcome::Eof { .. } => return None,
            ChunkOutcome::Pending { .. } => yield_macrotask().await,
        }
    }
}

#[kithara_test_utils::kithara::hang_watchdog]
async fn run_seek_pcm_window_check(mut audio: LaneAudio<Stream<Hls<TestPools>>, TestPools>) {
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let sample_rate = spec.sample_rate.get() as usize;
    let mut buf = vec![0.0f32; 4096];

    let warmup = warm_up(&mut audio, &mut buf).await;
    assert!(warmup > 0, "warmup must read some data");

    let duration_secs = audio
        .duration()
        .unwrap_or(Duration::from_secs(60))
        .as_secs_f64();
    let middle_secs = (duration_secs * 0.5).clamp(3.0, (duration_secs - 1.0).max(3.0));
    audio
        .seek(Duration::from_secs_f64(middle_secs))
        .expect("seek to middle must succeed");

    let mut played_frames = 0usize;
    let target_frames = sample_rate * 5;
    for _ in 0..600 {
        hang_tick!();
        let Some(n) = read_with_yield_limit(&mut audio, &mut buf, 50).await else {
            break;
        };
        if n == 0 {
            continue;
        }
        played_frames += n / channels;
        hang_reset!();
        if played_frames >= target_frames {
            break;
        }
    }
    assert!(
        played_frames >= sample_rate * 3,
        "expected at least ~3s playback before near-start seek"
    );

    let near_start = Duration::from_secs_f64(0.2);
    audio
        .seek(near_start)
        .expect("seek near start must succeed");

    let inspect_frames = (sample_rate * 35) / 100;
    let mut inspected = 0usize;
    let mut discontinuities = 0usize;
    let mut backward_jumps = 0usize;
    let mut first_bad: Option<(usize, usize, usize)> = None;
    let mut prev_phase: Option<usize> = None;

    for _ in 0..400 {
        hang_tick!();
        let Some(n) = read_with_yield_limit(&mut audio, &mut buf, 100).await else {
            break;
        };
        if n == 0 {
            continue;
        }

        let frames = n / channels;
        hang_reset!();
        for f in 0..frames {
            if inspected >= inspect_frames {
                break;
            }
            let phase = signal::phase::units(buf[f * channels]);
            if let Some(prev) = prev_phase {
                let expected = (prev + 1) % signal::SAW_PERIOD;
                if phase != expected {
                    discontinuities += 1;
                    if phase < prev {
                        backward_jumps += 1;
                    }
                    if first_bad.is_none() {
                        first_bad = Some((prev, phase, expected));
                    }
                }
            }
            prev_phase = Some(phase);
            inspected += 1;
        }

        if inspected >= inspect_frames {
            break;
        }
    }

    assert!(
        inspected >= inspect_frames,
        "inspected too few frames after seek: {inspected}/{inspect_frames}"
    );
    assert_eq!(
        discontinuities, 0,
        "discontinuities in early PCM window after seek: {discontinuities}, first_bad={first_bad:?}"
    );
    assert_eq!(
        backward_jumps, 0,
        "backward jumps in early PCM window after seek: {backward_jumps}, first_bad={first_bad:?}"
    );
}

/// Non-blocking read that yields to the event loop between attempts.
///
/// On wasm32 main thread, `Atomics.wait` is forbidden. The audio pipeline
/// uses `preload()` mode where `read()` returns 0 when data isn't ready yet.
/// We yield via `gloo_timers` to let async downloads and Web Workers proceed.
async fn read_with_yield(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    buf: &mut [f32],
) -> Option<usize> {
    read_with_yield_limit(audio, buf, 500).await
}

/// Pull the pipeline's first PCM, returning how many samples it produced.
///
/// A read that stays pending for its whole budget is a pipeline still
/// fetching, not a pipeline that has nothing to give: only the end of the
/// stream ends the warmup early. Collapsing the two would judge a slow
/// browser as a silent one.
#[kithara_test_utils::kithara::hang_watchdog]
async fn warm_up(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    buf: &mut [f32],
) -> usize {
    let mut produced = 0usize;
    for _ in 0..20 {
        hang_tick!();
        let Some(count) = read_with_yield(audio, buf).await else {
            break;
        };
        produced += count;
        if count > 0 {
            hang_reset!();
        }
    }
    produced
}

/// Read with a pending budget in wall-clock milliseconds. `None` is the end
/// of the stream; `Some(0)` is a reader that stayed pending for the whole
/// budget.
async fn read_with_yield_limit(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    buf: &mut [f32],
    budget_ms: u32,
) -> Option<usize> {
    read_outcome_with_yield_limit(audio, buf, budget_ms)
        .await
        .map(|outcome| match outcome {
            ReadOutcome::Frames { count, .. } => count.get(),
            _ => 0,
        })
}

#[kithara_test_utils::kithara::hang_watchdog]
async fn read_outcome_with_yield_limit(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    buf: &mut [f32],
    budget_ms: u32,
) -> Option<ReadOutcome> {
    let deadline = Date::now() + f64::from(budget_ms);
    loop {
        hang_tick!();
        let pending = match audio.read(buf) {
            Ok(outcome @ ReadOutcome::Frames { .. }) => {
                hang_reset!();
                return Some(outcome);
            }
            Ok(ReadOutcome::Eof { .. }) => return None,
            Ok(outcome @ ReadOutcome::Pending { .. }) => outcome,
            Err(error) => {
                warn!(%error, "read failed");
                return None;
            }
        };
        if Date::now() >= deadline {
            return Some(pending);
        }
        if audio.current_segment_ready() {
            yield_macrotask().await;
        } else if kithara::platform::time::timeout(
            Duration::from_secs_f64((deadline - Date::now()).max(0.0) / 1000.0),
            wait_for_preload(audio, "browser read"),
        )
        .await
        .is_err()
        {
            return Some(pending);
        }
    }
}

/// One macrotask turn, so fetch and worker messages make progress.
///
/// A timer turn is clamped to at least 4 ms once timers nest, which Firefox
/// applies to this loop; a message-port turn has no clamp, so the wait
/// tracks the pipeline instead of the browser's timer policy.
#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog]
async fn yield_macrotask() {
    let channel = MessageChannel::new().expect("MessageChannel is available in every worker");
    let turn = Promise::new(&mut |resolve, _| channel.port1().set_onmessage(Some(&resolve)));
    channel
        .port2()
        .post_message(&JsValue::NULL)
        .expect("posting to an owned port cannot fail");
    let mut budget = Duration::ZERO;
    hang_park!(|remaining| budget = remaining);
    kithara::platform::time::timeout(budget, JsFuture::from(turn))
        .await
        .expect("browser message turn must arrive")
        .expect("message turn resolves");
}

/// Yield to event loop so async I/O and Web Workers can progress.
#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog]
async fn yield_ms(ms: u32) {
    let mut budget = Duration::ZERO;
    hang_park!(|remaining| budget = remaining);
    kithara::platform::time::timeout(budget, TimeoutFuture::new(ms))
        .await
        .expect("browser timer turn must arrive");
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn stress_read_samples_integrity(#[future(awt)] stress_source: (TestServerHelper, Url)) {
    let (_helper, url) = stress_source;
    info!("Starting stress_read_samples_integrity");

    let mut audio = create_pipeline_with_url(url).await;
    let spec = audio.spec();

    assert!(spec.channels > 0, "channels must be > 0");
    assert!(spec.sample_rate.get() > 0, "sample_rate must be > 0");
    info!(
        channels = spec.channels,
        sample_rate = spec.sample_rate,
        "Audio spec"
    );

    let channels = spec.channels as usize;
    let mut buf = vec![0.0f32; 4096];
    let mut total_samples = 0usize;
    let target_chunks = 200;
    let mut chunks_read = 0;
    let mut channel_mismatches = 0u64;
    let mut continuity_errors = 0u64;
    let mut prev_last_phase: Option<usize> = None;

    for _ in 0..target_chunks {
        hang_tick!();
        let n = read_with_yield(&mut audio, &mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        chunks_read += 1;
        total_samples += n;
        hang_reset!();
        let frames = n / channels;

        for (i, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite(),
                "sample {i} in chunk {chunks_read} is not finite: {sample}"
            );
            assert!(
                (-1.0..=1.0).contains(&sample),
                "sample {i} in chunk {chunks_read} out of range: {sample}"
            );
        }

        if channels == 2 {
            for f in 0..frames {
                let l = buf[f * 2];
                let r = buf[f * 2 + 1];
                if (l - r).abs() > f32::EPSILON {
                    channel_mismatches += 1;
                    if channel_mismatches <= 3 {
                        warn!(chunk = chunks_read, frame = f, l, r, "L/R mismatch");
                    }
                }
            }
        }

        if let Some(prev_phase) = prev_last_phase {
            let first_phase = signal::phase::units(buf[0]);
            let expected = (prev_phase + 1) % signal::SAW_PERIOD;
            if first_phase != expected {
                continuity_errors += 1;
                if continuity_errors <= 3 {
                    warn!(
                        chunk = chunks_read,
                        prev_phase, first_phase, expected, "inter-chunk continuity break"
                    );
                }
            }
        }

        if frames >= 2 {
            for f in 1..frames {
                let prev = signal::phase::units(buf[(f - 1) * channels]);
                let curr = signal::phase::units(buf[f * channels]);
                let expected = (prev + 1) % signal::SAW_PERIOD;
                if curr != expected {
                    continuity_errors += 1;
                    if continuity_errors <= 3 {
                        warn!(
                            chunk = chunks_read,
                            frame = f,
                            prev,
                            curr,
                            expected,
                            "intra-chunk continuity break"
                        );
                    }
                }
            }
        }

        if frames > 0 {
            prev_last_phase = Some(signal::phase::units(buf[(frames - 1) * channels]));
        }
    }

    info!(
        total_samples,
        chunks_read, channel_mismatches, continuity_errors, "Read complete"
    );
    assert!(total_samples > 0, "must read at least some audio samples");
    assert!(
        chunks_read >= 5,
        "expected at least 5 chunks, got {chunks_read}"
    );
    assert_eq!(
        channel_mismatches, 0,
        "L/R channels diverged {channel_mismatches} times — data corruption"
    );
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks (>5 tolerance) — non-contiguous decoded data"
    );
}

fn saw_phase_at(span: SourceSpan, frame: u64) -> usize {
    let (numerator, denominator) = span.source_ratio_at(frame).expect("mapped saw frame");
    let denominator = denominator.get();
    let phase = usize::try_from((numerator / denominator) % signal::SAW_PERIOD as u128)
        .expect("saw phase fits usize");
    let tap = |phase| {
        f64::from(signal::Wave::Sawtooth.sample(phase, span.sample_rate().get())) / 32_768.0
    };
    let current = tap(phase);
    let next = tap((phase + 1) % signal::SAW_PERIOD);
    let fraction = (numerator % denominator).to_f64().expect("source fraction")
        / denominator.to_f64().expect("source denominator");
    let expected = ((next - current) * fraction + current)
        .to_f32()
        .expect("finite interpolated saw sample");
    signal::phase::units(expected)
}

fn saw_pair_continuous(span: SourceSpan, frame: u64, previous: usize, current: usize) -> bool {
    previous == saw_phase_at(span, frame - 1) && current == saw_phase_at(span, frame)
}

#[kithara::test(wasm)]
fn fractional_saw_wrap_oracle_rejects_repeated_dropped_and_zeroed_samples() {
    let denominator = 2_500_000_u128;
    let span = SourceSpan::try_from((
        65_536 * denominator - 81,
        denominator,
        std::num::NonZeroU128::new(denominator).expect("source denominator"),
        std::num::NonZeroU32::new(44_100).expect("source rate"),
        4,
    ))
    .expect("fractional saw source span");
    let samples = [-0.999_935_2_f32, -0.999_969_5, -0.999_938_96, -0.999_908_45];
    let errors = |samples: &[f32]| {
        samples
            .windows(2)
            .enumerate()
            .filter(|(frame, pair)| {
                !saw_pair_continuous(
                    span,
                    (*frame + 1) as u64,
                    signal::phase::units(pair[0]),
                    signal::phase::units(pair[1]),
                )
            })
            .count()
    };
    assert_eq!(errors(&samples), 0, "fractional wrap is continuous");
    for corrupted in [samples[0], samples[2], 0.0] {
        let mut broken = samples;
        broken[1] = corrupted;
        assert_eq!(
            errors(&broken),
            2,
            "both adjacent pairs must detect corruption"
        );
    }
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn stress_seek_and_read(#[future(awt)] stress_source: (TestServerHelper, Url)) {
    let (_helper, url) = stress_source;
    info!("Starting stress_seek_and_read");

    let mut audio = create_pipeline_with_url(url).await;

    let mut buf = vec![0.0f32; 4096];
    let warmup = warm_up(&mut audio, &mut buf).await;
    assert!(warmup > 0, "warmup must read some data");
    info!(warmup, "Pipeline warmed up");

    let duration = audio.duration().unwrap_or(Duration::from_secs(30));
    let duration_secs = duration.as_secs_f64();

    let seek_positions = [0.1, 0.5, 0.25, 0.75, 0.0, 0.9, 0.3, 0.6, 0.15, 0.85];
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let mut position_errors = 0u64;
    let mut channel_mismatches = 0u64;
    let mut continuity_errors = 0u64;
    let mut first_breaks = Vec::new();

    for (i, &frac) in seek_positions.iter().enumerate() {
        hang_tick!();
        let pos_secs = duration_secs * frac;
        let seek_to = Duration::from_secs_f64(pos_secs);
        info!(seek_idx = i, seek_ms = seek_to.as_millis(), "Seeking");

        if let Err(e) = audio.seek(seek_to) {
            info!(?e, "Seek failed (may be near boundary), skipping");
            continue;
        }

        let mut post_seek_samples = 0;
        let mut position_checked = false;
        for read_idx in 0..10 {
            hang_tick!();
            let Some(ReadOutcome::Frames {
                count, source_span, ..
            }) = read_outcome_with_yield_limit(&mut audio, &mut buf, 500).await
            else {
                break;
            };
            let n = count.get();
            let frames = n / channels;
            let span = source_span.expect("rendered seek PCM has exact source geometry");
            assert_eq!(span.output_frames(), frames as u64);
            assert_eq!(span.sample_rate(), spec.sample_rate);
            hang_reset!();
            post_seek_samples += n;

            for &sample in &buf[..n] {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "corrupt sample after seek {i}: {sample}"
                );
            }

            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    if (l - r).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                    }
                }
            }

            if frames >= 2 {
                for f in 1..frames {
                    let prev = signal::phase::units(buf[(f - 1) * channels]);
                    let curr = signal::phase::units(buf[f * channels]);
                    if !saw_pair_continuous(span, f as u64, prev, curr) {
                        continuity_errors += 1;
                        if first_breaks.len() < 8 {
                            first_breaks.push(format!(
                                "seek={i} target={seek_to:?} read={read_idx} frame={f}/{frames} previous_sample={} current_sample={} previous_phase={prev} current_phase={curr} position={:?}",
                                buf[(f - 1) * channels],
                                buf[f * channels],
                                audio.position(),
                            ));
                        }
                    }
                }
            }

            if !position_checked && frames > 0 {
                let expected_frame = (pos_secs * spec.sample_rate.get() as f64).round() as usize;
                let expected_phase = expected_frame % signal::SAW_PERIOD;
                let actual_phase = signal::phase::units(buf[0]);
                let dist = signal::phase::distance(actual_phase, expected_phase);
                if dist > 1200 {
                    position_errors += 1;
                    warn!(
                        seek_idx = i,
                        pos_secs,
                        expected_phase,
                        actual_phase,
                        dist,
                        "position mismatch after seek"
                    );
                }
                position_checked = true;
            }
        }
        info!(post_seek_samples, "Read after seek {i}");
    }

    info!(
        position_errors,
        channel_mismatches, continuity_errors, "Seek test done"
    );
    assert_eq!(
        channel_mismatches, 0,
        "L/R channels diverged {channel_mismatches} times"
    );
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks (>5 tolerance); first={first_breaks:?}"
    );
    assert!(
        position_errors <= 1,
        "{position_errors} position mismatches — seek landed in wrong place"
    );
}

/// Aggressive seek stress test: 1000 rapid random seeks.
///
/// Catches the bug where `read()` returns 0 after seek and the pipeline
/// stalls permanently (player stops, never resumes without stop+restart).
///
/// Strategy:
/// - Warmup: read 20 chunks to prime the pipeline
/// - 1000 random seeks: 10% near start (<1s), 10% near end, 80% random
/// - After each seek: read_with_yield must produce >0 samples (not stuck)
/// - All samples must be finite and in [-1.0, 1.0]
/// - Tolerate at most 1% dead seeks (pipeline restart race)
#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn stress_rapid_seeks_must_not_stall(#[future(awt)] stress_source: (TestServerHelper, Url)) {
    let (_helper, url) = stress_source;
    info!("Starting stress_rapid_seeks_must_not_stall");
    wasm_bindgen_test::console_log!("rapid seek body starts at {} ms", Date::now());

    let mut audio = create_pipeline_with_url(url).await;
    let spec = audio.spec();
    info!(
        channels = spec.channels,
        sample_rate = spec.sample_rate,
        "Pipeline created"
    );

    let mut buf = vec![0.0f32; 4096];

    let warmup_samples = warm_up(&mut audio, &mut buf).await;
    assert!(warmup_samples > 0, "warmup must produce data");
    info!(warmup_samples, "Warmup complete");
    wasm_bindgen_test::console_log!("rapid seek warmup done at {} ms", Date::now());

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();
    let max_seek = duration_secs - 0.5;
    info!(duration_secs, max_seek, "Duration known");

    const SEEK_COUNT: usize = 1000;
    let sample_rate = audio.spec().sample_rate.get();
    let channels = audio.spec().channels as usize;
    let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
    let mut dead_seeks = 0u64;
    let mut seek_errors = 0u64;
    let mut integrity_errors = 0u64;
    let mut position_mismatches = 0u64;
    let mut total_samples = 0u64;

    let mut seek_events = audio.events::<AudioEvent>();
    let mut decoder_events = audio.events::<DecoderEvent>();
    for i in 0..SEEK_COUNT {
        hang_tick!();
        let r = rng.next_f64();
        let pos_secs = if r < 0.1 {
            rng.range_f64(0.0, 1.0_f64.min(max_seek))
        } else if r < 0.2 {
            rng.range_f64((max_seek - 2.0).max(0.0), max_seek)
        } else {
            rng.range_f64(0.001, max_seek)
        };

        let position = Duration::from_secs_f64(pos_secs);
        if let Err(e) = audio.seek(position) {
            seek_errors += 1;
            if seek_errors <= 3 {
                warn!(iteration = i, pos_secs, ?e, "seek error");
            }
            continue;
        }

        let outcome = read_outcome_with_yield_limit(&mut audio, &mut buf, 200).await;
        let read = outcome.as_ref().map(|outcome| match outcome {
            ReadOutcome::Frames { count, .. } => count.get(),
            _ => 0,
        });
        let n = read.unwrap_or(0);
        if n == 0 && read.is_some() {
            dead_seeks += 1;
            if dead_seeks <= 5 {
                let stages: Vec<_> = std::iter::from_fn(|| seek_events.try_recv().ok())
                    .take(16)
                    .collect();
                let decoders: Vec<_> = std::iter::from_fn(|| decoder_events.try_recv().ok())
                    .take(16)
                    .collect();
                wasm_bindgen_test::console_log!(
                    "rapid seek stalled iteration={} requested={} outcome={:?} segment={:?} committed={:?} stages={:?} decoders={:?}",
                    i,
                    pos_secs,
                    outcome,
                    audio.segment(),
                    audio.committed_segment(),
                    stages,
                    decoders,
                );
                warn!(iteration = i, pos_secs, "STALL: read returned 0 after seek");
            }
            continue;
        }

        for &sample in &buf[..n] {
            if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
                integrity_errors += 1;
                break;
            }
        }

        let frames = n / channels;
        if frames > 0 {
            hang_reset!();
            let expected_frame = (pos_secs * sample_rate as f64).round() as usize;
            let expected_phase = expected_frame % signal::SAW_PERIOD;
            let actual_phase = signal::phase::units(buf[0]);
            let dist = signal::phase::distance(actual_phase, expected_phase);
            if dist > 1200 {
                position_mismatches += 1;
                if position_mismatches <= 5 {
                    warn!(
                        iteration = i,
                        pos_secs, expected_phase, actual_phase, dist, "position mismatch"
                    );
                }
            }
        }

        total_samples += n as u64;

        if (i + 1) % 200 == 0 {
            wasm_bindgen_test::console_log!(
                "rapid seek progress iteration={} dead={} seek_errors={} samples={} at={} ms",
                i + 1,
                dead_seeks,
                seek_errors,
                total_samples,
                Date::now(),
            );
            info!(
                iteration = i + 1,
                dead_seeks, seek_errors, position_mismatches, total_samples, "Progress"
            );
        }

        if i % 50 == 49 {
            yield_ms(1).await;
        }
    }

    info!(
        dead_seeks,
        seek_errors,
        integrity_errors,
        position_mismatches,
        total_samples,
        "Phase 2 complete: {SEEK_COUNT} seeks"
    );

    let max_dead = (SEEK_COUNT as u64) / 100;
    assert!(
        dead_seeks <= max_dead,
        "pipeline stalled {dead_seeks}/{SEEK_COUNT} times \
         (>{max_dead} = 1% threshold) — read() returns 0 after seek"
    );
    assert_eq!(
        integrity_errors, 0,
        "samples outside [-1,1] or not finite — data corruption"
    );

    let max_mismatches = (SEEK_COUNT as u64) / 20;
    assert!(
        position_mismatches <= max_mismatches,
        "position mismatches {position_mismatches}/{SEEK_COUNT} \
         (>{max_mismatches} = 5% threshold) — seek landed in wrong place"
    );
}

/// Seek-to-zero after heavy pressure: ensures the beginning of the track
/// is still accessible after thousands of operations.
///
/// Catches the bug where seeking back to position 0 produces no data
/// because first segments were evicted or the pipeline entered a bad state.
///
/// Strategy:
/// - Warmup: read 20 chunks
/// - Stress: 500 random seeks (prime the pipeline, exercise eviction)
/// - Reset: seek to 0
/// - Verify: read at least 50 chunks from the beginning, all valid
/// - Position must be near 0 after seeking, then advance monotonically
#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn stress_seek_to_zero_after_pressure(#[future(awt)] stress_source: (TestServerHelper, Url)) {
    let (_helper, url) = stress_source;
    info!("Starting stress_seek_to_zero_after_pressure");
    wasm_bindgen_test::console_log!("seek-zero body starts at {} ms", Date::now());

    let mut audio = create_pipeline_with_url(url).await;
    let mut buf = vec![0.0f32; 4096];

    let warmup = warm_up(&mut audio, &mut buf).await;
    assert!(warmup > 0, "warmup must produce data");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();
    let max_seek = duration_secs - 0.5;
    info!(warmup, duration_secs, "Warmup done");

    let mut rng = Xorshift64::new(0xABCD_EF01_2345_6789);
    let mut seek_events = audio.events::<AudioEvent>();
    let mut decoder_events = audio.events::<DecoderEvent>();
    for i in 0..500 {
        hang_tick!();
        let pos = rng.range_f64(0.001, max_seek);
        let _ = audio.seek(Duration::from_secs_f64(pos));
        let outcome = read_outcome_with_yield_limit(&mut audio, &mut buf, 50).await;
        let count = outcome.as_ref().map_or(0, |outcome| match outcome {
            ReadOutcome::Frames { count, .. } => count.get(),
            _ => 0,
        });
        if count == 0 && i < 5 {
            let stages: Vec<_> = std::iter::from_fn(|| seek_events.try_recv().ok())
                .take(16)
                .collect();
            let decoders: Vec<_> = std::iter::from_fn(|| decoder_events.try_recv().ok())
                .take(16)
                .collect();
            wasm_bindgen_test::console_log!(
                "seek-zero pressure stalled iteration={} requested={} outcome={:?} segment={:?} committed={:?} stages={:?} decoders={:?}",
                i,
                pos,
                outcome,
                audio.segment(),
                audio.committed_segment(),
                stages,
                decoders,
            );
        }
        if count > 0 {
            hang_reset!();
        }

        if i % 100 == 99 {
            wasm_bindgen_test::console_log!(
                "seek-zero pressure iteration={} at={} ms",
                i + 1,
                Date::now()
            );
            yield_ms(1).await;
        }
    }
    wasm_bindgen_test::console_log!("seek-zero pressure done at={} ms", Date::now());
    info!("Stress phase done: 500 seeks");

    let channels = audio.spec().channels as usize;
    let sample_rate = audio.spec().sample_rate;
    audio
        .seek(Duration::ZERO)
        .expect("seek to 0 must succeed after stress");

    let pos_after_seek = audio.position();
    info!(
        pos_ms = pos_after_seek.as_millis(),
        "Seeked to 0, position reported"
    );

    let mut total_from_zero = 0usize;
    let mut chunks_from_zero = 0usize;
    let target_chunks = 50;
    let mut position_checked = false;
    let mut continuity_errors = 0u64;
    let mut prev_last_phase: Option<usize> = None;
    let mut stall_deadline = Instant::now() + Duration::from_secs(25);

    for attempt in 0..target_chunks * 20 {
        hang_tick!();
        let Some(n) = read_with_yield_limit(&mut audio, &mut buf, 100).await else {
            info!(chunks_from_zero, total_from_zero, "EOF reached");
            break;
        };
        if n == 0 {
            if Instant::now() >= stall_deadline {
                panic!(
                    "STUCK after seek-to-0: read returned 0 for {attempt} attempts before deadline, \
                     chunks_from_zero={chunks_from_zero}, \
                     position={:.3}s",
                    audio.position().as_secs_f64()
                );
            }
            continue;
        }

        stall_deadline = Instant::now() + Duration::from_secs(5);

        chunks_from_zero += 1;
        total_from_zero += n;
        hang_reset!();
        let frames = n / channels;

        for (j, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "corrupt sample at offset {j} in chunk {chunks_from_zero} \
                 after seek-to-0: {sample}"
            );
        }

        if !position_checked && frames > 0 {
            let actual_phase = signal::phase::units(buf[0]);
            let dist = signal::phase::distance(actual_phase, 0);
            info!(
                actual_phase,
                dist, sample_rate, "Phase after seek-to-0 (expected ≈ 0)"
            );
            assert!(
                dist <= 1200,
                "after seek(0), decoded phase is {actual_phase} \
                 (distance {dist} > 1200 from expected 0) — seek landed in wrong place"
            );
            position_checked = true;
        }

        if let Some(prev_phase) = prev_last_phase {
            let first_phase = signal::phase::units(buf[0]);
            if first_phase != (prev_phase + 1) % signal::SAW_PERIOD {
                continuity_errors += 1;
            }
        }

        if frames >= 2 {
            for f in 1..frames {
                let prev = signal::phase::units(buf[(f - 1) * channels]);
                let curr = signal::phase::units(buf[f * channels]);
                if curr != (prev + 1) % signal::SAW_PERIOD {
                    continuity_errors += 1;
                }
            }
        }

        if frames > 0 {
            prev_last_phase = Some(signal::phase::units(buf[(frames - 1) * channels]));
        }

        if chunks_from_zero >= target_chunks {
            break;
        }
    }

    info!(
        chunks_from_zero,
        total_from_zero,
        continuity_errors,
        position_ms = audio.position().as_millis(),
        "Read after seek-to-0 complete"
    );

    assert!(
        chunks_from_zero >= target_chunks,
        "expected at least {target_chunks} chunks after seek-to-0, got {chunks_from_zero} \
         — first segments may be missing or pipeline stalled"
    );
    assert!(total_from_zero > 0, "must read samples after seek-to-0");
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks after seek-to-0 — non-contiguous data"
    );
}

/// Regression: after long playback from the middle, seek near start (but not 0)
/// must land inside segment 0, not at segment 1 boundary.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn stress_seek_near_start_after_mid_playback_must_land_inside_first_segment(
    #[future(awt)] stress_source: (TestServerHelper, Url),
) {
    let (_helper, url) = stress_source;
    info!("Starting stress_seek_near_start_after_mid_playback_must_land_inside_first_segment");

    let mut audio = create_pipeline_with_url(url).await;
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let sample_rate = spec.sample_rate.get() as usize;
    let mut buf = vec![0.0f32; 4096];

    let warmup = warm_up(&mut audio, &mut buf).await;
    assert!(warmup > 0, "warmup must read some data");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();

    let middle_secs = (duration_secs * 0.5).clamp(3.0, (duration_secs - 1.0).max(3.0));
    audio
        .seek(Duration::from_secs_f64(middle_secs))
        .expect("seek to middle must succeed");

    let mut played_frames = 0usize;
    let target_frames = sample_rate * 5;
    for _ in 0..600 {
        hang_tick!();
        let Some(n) = read_with_yield_limit(&mut audio, &mut buf, 50).await else {
            break;
        };
        if n == 0 {
            continue;
        }
        played_frames += n / channels;
        if played_frames >= target_frames {
            hang_reset!();
            break;
        }
        hang_reset!();
    }
    assert!(
        played_frames >= sample_rate * 3,
        "expected at least ~3s playback before near-start seek, got {} frames",
        played_frames
    );

    let near_start_secs = 0.2_f64;
    audio
        .seek(Duration::from_secs_f64(near_start_secs))
        .expect("seek near start must succeed");

    let expected_frame = (near_start_secs * spec.sample_rate.get() as f64).round() as usize;
    let expected_phase = expected_frame % signal::SAW_PERIOD;
    let seg1_start_frames = 200_000 / (channels * 2);
    let seg1_start_phase = seg1_start_frames % signal::SAW_PERIOD;

    let mut checked = false;
    for _ in 0..200 {
        hang_tick!();
        let n = read_with_yield_limit(&mut audio, &mut buf, 100)
            .await
            .unwrap_or(0);
        if n == 0 {
            continue;
        }
        let frames = n / channels;
        if frames == 0 {
            continue;
        }

        let actual_phase = signal::phase::units(buf[0]);
        hang_reset!();
        let dist_expected = signal::phase::distance(actual_phase, expected_phase);
        let dist_seg1 = signal::phase::distance(actual_phase, seg1_start_phase);

        info!(
            near_start_secs,
            expected_phase,
            actual_phase,
            dist_expected,
            seg1_start_phase,
            dist_seg1,
            "phase after middle->near-start seek"
        );

        assert!(
            dist_expected <= 1200,
            "near-start seek landed in wrong place: \
             expected_phase={expected_phase}, actual_phase={actual_phase}, dist={dist_expected}"
        );
        assert!(
            dist_seg1 > 3000,
            "near-start seek suspiciously close to segment-1 boundary: \
             actual_phase={actual_phase}, seg1_phase={seg1_start_phase}, dist={dist_seg1}"
        );

        checked = true;
        break;
    }

    assert!(
        checked,
        "failed to decode samples after near-start seek following middle playback"
    );
}

/// TestEvent-level regression guard for seek behavior in browser path:
/// one seek command should produce one seek-complete, and playback progress
/// after that seek should advance without extra backward resets.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn stress_seek_events_single_reset_and_monotonic_progress(
    #[future(awt)] stress_source: (TestServerHelper, Url),
) {
    let (_helper, url) = stress_source;
    info!("Starting stress_seek_events_single_reset_and_monotonic_progress");

    let (worker, config) = stress_pipeline_config(url);
    let mut audio = {
        let _real_io = kithara::platform::flash::real_io();
        let mut budget = Duration::ZERO;
        hang_park!(|remaining| budget = remaining);
        kithara::platform::time::timeout(
            budget,
            kithara_integration_tests::mock::load_source_audio(&worker, config),
        )
        .await
        .expect("source-open watchdog")
        .expect("open the committed source")
    };
    hang_reset!();
    let mut events_rx = audio.event_bus().subscribe::<AudioEvent>();
    let spec = audio.spec();
    let channels = spec.channels as usize;

    let mut warmup = 0;
    for _ in 0..20 {
        hang_tick!();
        let Some(count) = read_committed_source(&mut audio).await else {
            break;
        };
        warmup += count;
        hang_reset!();
    }
    assert!(warmup > 0, "warmup must read some data");

    while events_rx.try_recv().is_ok() {
        hang_tick!();
        hang_reset!();
    }

    let duration_secs = audio
        .duration()
        .unwrap_or(Duration::from_secs(60))
        .as_secs_f64();
    let middle_secs = (duration_secs * 0.5).clamp(3.0, (duration_secs - 1.0).max(3.0));
    AudioSource::seek(&mut audio, Duration::from_secs_f64(middle_secs))
        .expect("seek to middle must succeed");

    let mut played_frames = 0usize;
    let target_frames = spec.sample_rate.get() as usize * 5;
    for _ in 0..600 {
        hang_tick!();
        let Some(n) = read_committed_source(&mut audio).await else {
            break;
        };
        if n == 0 {
            continue;
        }
        played_frames += n / channels;
        hang_reset!();
        if played_frames >= target_frames {
            break;
        }
    }
    assert!(
        played_frames >= spec.sample_rate.get() as usize * 3,
        "expected at least ~3s playback before test seek"
    );

    while events_rx.try_recv().is_ok() {
        hang_tick!();
        hang_reset!();
    }

    let near_start_secs = 0.2_f64;
    let near_start = Duration::from_secs_f64(near_start_secs);
    AudioSource::seek(&mut audio, near_start).expect("seek near start must succeed");

    let mut target_seek_request = None;
    let mut seek_complete_for_target = 0usize;
    let mut seek_complete_seen = false;
    let mut playback_positions = Vec::with_capacity(64);

    for _ in 0..300 {
        hang_tick!();
        if read_committed_source(&mut audio)
            .await
            .is_some_and(|count| count > 0)
        {
            hang_reset!();
        }

        loop {
            hang_tick!();
            let Ok(envelope) = events_rx.try_recv() else {
                break;
            };
            hang_reset!();
            match envelope.event {
                AudioEvent::SeekLifecycle {
                    stage: SeekLifecycleStage::SeekRequest,
                    ..
                } => {
                    assert!(
                        target_seek_request.is_none(),
                        "one request identifies the target seek"
                    );
                    target_seek_request = Some((envelope.meta.origin, envelope.meta.seq));
                }
                AudioEvent::SeekComplete { .. } => {
                    if target_seek_request.is_some_and(|(origin, seq)| {
                        origin == envelope.meta.origin && seq < envelope.meta.seq
                    }) {
                        seek_complete_for_target += 1;
                        seek_complete_seen = true;
                        playback_positions.clear();
                    }
                }
                AudioEvent::PlaybackProgress { position_ms, .. } => {
                    if seek_complete_seen
                        && target_seek_request.is_some_and(|(origin, seq)| {
                            origin == envelope.meta.origin && seq < envelope.meta.seq
                        })
                    {
                        playback_positions.push(position_ms);
                    }
                }
                _ => {}
            }
        }

        if seek_complete_seen && playback_positions.len() >= 12 {
            break;
        }
    }

    assert!(
        target_seek_request.is_some(),
        "no AudioEvent::SeekLifecycle::SeekRequest for target seek"
    );
    assert!(
        seek_complete_for_target >= 1,
        "no AudioEvent::SeekComplete for target seek"
    );
    assert_eq!(
        seek_complete_for_target, 1,
        "single seek produced multiple seek-complete events for same target"
    );
    assert!(
        playback_positions.len() >= 8,
        "insufficient PlaybackProgress events after seek: {}",
        playback_positions.len()
    );

    let mut regressions = 0usize;
    let mut prev = None;
    for pos in playback_positions {
        if let Some(prev_pos) = prev
            && pos < prev_pos
        {
            regressions += 1;
        }
        prev = Some(pos);
    }

    assert_eq!(
        regressions, 0,
        "PlaybackProgress moved backward {regressions} times after a single seek"
    );
}

/// PCM-level regression guard for audible "micro-loop" after seek.
///
/// Scenario:
/// - Seek to middle, read ~5s
/// - Seek near start (non-zero)
/// - Inspect the first ~350ms of decoded PCM after seek
///
/// For the deterministic saw-tooth fixture, this early window must be strictly
/// contiguous frame-by-frame (no tiny backward jumps / repeated fragments).
#[kithara::test(wasm, serial, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
async fn stress_seek_pcm_window_after_seek_must_not_loop_fragment(
    #[future(awt)] stress_source: (TestServerHelper, Url),
) {
    let (_helper, url) = stress_source;
    info!("Starting stress_seek_pcm_window_after_seek_must_not_loop_fragment");

    run_seek_pcm_window_check(create_pipeline_with_url(url).await).await;
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)), hang_timeout_secs(1))]
async fn stress_seek_pcm_window_after_seek_must_not_loop_fragment_jitter(
    #[future(awt)] jitter_source: (TestServerHelper, Url),
) {
    let (_helper, url) = jitter_source;
    info!("Starting stress_seek_pcm_window_after_seek_must_not_loop_fragment_jitter");

    run_seek_pcm_window_check(create_pipeline_with_url(url).await).await;
}

#[kithara::fixture]
async fn stress_source() -> (TestServerHelper, Url) {
    create_stress_source(false).await
}
#[kithara::fixture]
async fn jitter_source() -> (TestServerHelper, Url) {
    create_stress_source(true).await
}
