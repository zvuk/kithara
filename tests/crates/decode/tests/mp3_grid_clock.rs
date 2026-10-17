#![cfg(not(target_os = "android"))]

use std::io::Cursor;

use ::kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{
        Audio, AudioConfig, AudioDecoderConfig, AudioEvent, AudioSource, ChunkOutcome,
        DecoderBackend as ObservedBackend, DecoderChangeCause, DecoderEvent, SeekLifecycleStage,
        SeekOutcome,
    },
    decode::{DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, GaplessMode},
    events::EventBus,
    file::{File, FileConfig, FileSrc},
    platform::{
        sync::Arc,
        thread,
        time::Duration,
        tokio::task::{spawn_blocking, yield_now},
    },
    resampler::NoResamplerBackend,
    signal::{AudioChunk, AudioChunkInfo},
    stream::{AudioCodec, mock::NoopWorkerWake},
};
use kithara_integration_tests::{
    bufpool_ext::{TestPools, pools},
    cochlea::marked_rhythm_markers,
};
use kithara_test_fixtures::assets::{rhythm_mp3_deck_b_120bpm_48k, rhythm_wav_deck_b_120bpm_48k};
use kithara_test_utils::kithara;

const CHANNELS: u16 = 2;
const PERIOD: u64 = 24_000;
const RATE: u32 = 48_000;
const WINDOW: usize = 48_000 * 3;

#[derive(Default)]
struct Capture {
    chunks: Vec<(usize, AudioChunkInfo)>,
    samples: Vec<f32>,
}

impl Capture {
    fn frames(&self) -> usize {
        self.samples.len() / usize::from(CHANNELS)
    }

    fn push(&mut self, chunk: &AudioChunk) {
        assert_eq!(chunk.spec().sample_rate.get(), RATE);
        assert_eq!(chunk.spec().channels, CHANNELS);
        assert_eq!(chunk.samples.len(), chunk.frames() * usize::from(CHANNELS));
        assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
        self.chunks.push((self.frames(), chunk.meta));
        self.samples.extend_from_slice(&chunk.samples);
    }

    fn assert_media_phase(&self, original: bool, origin: Duration, score: &[usize]) {
        let (markers, _) = marked_rhythm_markers(&self.samples, CHANNELS, RATE);
        let mut media_markers = Vec::new();
        for marker in markers {
            let index = self.chunks.partition_point(|(at, _)| *at <= marker) - 1;
            let (at, meta) = self.chunks[index];
            assert_eq!(meta.media_origin, Some(origin));
            let relative = u64::try_from(marker - at).expect("bounded marker offset");
            let raw = meta.source_span.map_or_else(
                || meta.timestamp + meta.spec.duration_for(relative).expect("marker time"),
                |span| span.position_at(relative).expect("marker source span"),
            );
            let media = raw
                .checked_sub(origin)
                .expect("musical marker follows media zero");
            media_markers.push(meta.spec.frame_at(media).expect("media marker frame"));
        }
        assert!(media_markers.len() >= 3);
        assert!(media_markers.iter().all(|frame| frame % PERIOD == 0));
        if original {
            assert_eq!(
                &media_markers[..score.len()],
                score
                    .iter()
                    .map(|frame| u64::try_from(*frame).expect("score marker"))
                    .collect::<Vec<_>>()
                    .as_slice(),
            );
        }
    }

    fn report(&self, label: &str, backend: DecoderBackend, mode: GaplessMode) {
        let (markers, downbeats) = marked_rhythm_markers(&self.samples, CHANNELS, RATE);
        let source_markers: Vec<_> = markers
            .iter()
            .map(|frame| {
                let index = self.chunks.partition_point(|(at, _)| *at <= *frame) - 1;
                let (at, meta) = self.chunks[index];
                let relative = u64::try_from(frame - at).expect("marker fits source clock");
                let raw_frame = meta.frame_offset + relative;
                let raw_seconds = meta.source_span.map_or_else(
                    || {
                        (meta.timestamp
                            + meta
                                .spec
                                .duration_for(relative)
                                .expect("raw marker timestamp"))
                        .as_secs_f64()
                    },
                    |span| {
                        span.seconds_at(relative)
                            .expect("marker lies in its source span")
                    },
                );
                (*frame, raw_frame, raw_seconds, raw_frame % PERIOD)
            })
            .collect();
        let frames = self.frames();
        let first_chunks = &self.chunks[..self.chunks.len().min(4)];
        tracing::info!(
            label,
            ?backend,
            ?mode,
            frames,
            ?first_chunks,
            ?source_markers,
            ?downbeats,
            "MP3_CLOCK_PCM"
        );
        assert!(
            markers.len() >= 3,
            "the unchanged marker oracle must identify the recording"
        );
    }
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(2),
    tracing("info")
)]
#[kithara::hang_watchdog(ctx = String)]
#[case::symphonia_disabled(
    DecoderBackend::Symphonia,
    ObservedBackend::Symphonia,
    GaplessMode::Disabled
)]
#[case::symphonia_media_only(
    DecoderBackend::Symphonia,
    ObservedBackend::Symphonia,
    GaplessMode::MediaOnly
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_disabled(DecoderBackend::Apple, ObservedBackend::Apple, GaplessMode::Disabled)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_media_only(DecoderBackend::Apple, ObservedBackend::Apple, GaplessMode::MediaOnly)
)]
async fn original_and_sought_markers_expose_the_raw_media_basis(
    #[case] backend: DecoderBackend,
    #[case] expected: ObservedBackend,
    #[case] mode: GaplessMode,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let encoded = rhythm_mp3_deck_b_120bpm_48k();
    let waveform = rhythm_wav_deck_b_120bpm_48k();
    let header = kithara_test_fixtures::signal::header(RATE, CHANNELS, Some(0));
    let authored: Vec<_> = waveform.bytes()[header.len()..]
        .as_chunks::<{ size_of::<i16>() }>()
        .0
        .iter()
        .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / f32::from(i16::MAX))
        .take(WINDOW * usize::from(CHANNELS))
        .collect();
    let score = marked_rhythm_markers(&authored, CHANNELS, RATE);
    tracing::info!(?backend, ?mode, ?score, "MP3_CLOCK_SOURCE_SCORE");
    assert!(score.0.len() >= 3);

    let pools = pools();
    let encoded_bytes = encoded.bytes().to_vec();
    let raw_pools = pools.clone();
    let (decoded, media_origin) = spawn_blocking(move || {
        let mut raw = DecoderFactory::create_with_probe(
            Cursor::new(encoded_bytes),
            Some("mp3"),
            DecoderConfig::<NoResamplerBackend, TestPools>::builder()
                .backend(backend)
                .gapless(!matches!(mode, GaplessMode::Disabled))
                .pools(raw_pools)
                .build(),
        )
        .expect("explicit raw decoder");
        let mut decoded = Capture::default();
        let mut watchdog = kithara_test_utils::hang::HangDetector::<String>::new(
            concat!(
                module_path!(),
                "::original_and_sought_markers_expose_the_raw_media_basis::native"
            ),
            kithara_test_utils::hang::default_timeout(),
        );
        while decoded.frames() < WINDOW {
            watchdog.tick_with_from(
                || format!("raw {backend:?} {mode:?} frames={}", decoded.frames()),
                file!(),
                line!(),
            );
            match raw.next_chunk().expect("raw original PCM") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    decoded.push(&chunk);
                    watchdog.reset_from(file!(), line!());
                }
                DecoderChunkOutcome::Pending(_) => thread::yield_now(),
                DecoderChunkOutcome::Eof => panic!("the three-second diagnostic window is not EOF"),
            }
        }
        let media_origin = raw.media_origin().expect("native recording media basis");
        let origin = raw.initial_pcm_origin();
        let track = raw.track_info();
        let gap = raw.timeline_gap_frames();
        let profile = raw.gapless_profile(Some(AudioCodec::Mp3));
        tracing::info!(
            ?backend,
            ?mode,
            ?origin,
            ?track,
            ?gap,
            ?profile,
            "MP3_CLOCK_RAW_DECODER"
        );
        (decoded, media_origin)
    })
    .await
    .expect("raw MP3 clock decoder task completes");
    decoded.report("raw-original", backend, mode);
    decoded.assert_media_phase(true, media_origin, &score.0);

    let bus = EventBus::default();
    let mut events = bus.subscribe::<DecoderEvent>();
    let file = FileConfig::for_src(FileSrc::Local(encoded.path().expect("stored MP3 fixture")))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file)
        .hint("mp3".to_owned())
        .events(bus)
        .decoder(
            AudioDecoderConfig::builder()
                .backend(backend)
                .gapless_mode(mode)
                .build(),
        )
        .build();
    let mut audio = Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
        .await
        .expect("explicit Audio source");
    let event = events
        .try_recv()
        .expect("real initial decoder receipt")
        .event;
    tracing::info!(?backend, ?mode, ?event, "MP3_CLOCK_INITIAL_DECODER");
    let DecoderEvent::DecoderChanged {
        backend: actual,
        cause: DecoderChangeCause::Initial,
        ..
    } = event
    else {
        panic!("initial decoder receipt required: {event:?}");
    };
    assert_eq!(actual, expected);
    assert_eq!(audio.media_origin(), Some(media_origin));

    for sought in [false, true] {
        let mut audio_events = audio.events::<AudioEvent>();
        let mut stages = Vec::new();
        let mut completions = Vec::new();
        let target = Duration::from_secs(2);
        if sought {
            let before_seek = audio.position();
            let result =
                AudioSource::seek(&mut audio, target).expect("same uncompensated raw seek");
            tracing::info!(?backend, ?mode, ?target, ?result, "MP3_CLOCK_RAW_SEEK");
            match result {
                SeekOutcome::Landed {
                    target: asked,
                    landed_at,
                } => {
                    assert_eq!(asked, target);
                    assert!(landed_at <= target);
                }
                SeekOutcome::Pending { target: asked, .. } => {
                    assert_eq!(asked, target);
                    assert_eq!(audio.position(), before_seek);
                }
                other @ SeekOutcome::PastEof { .. } => {
                    panic!("two-second seek must retain a playable suffix: {other:?}")
                }
            }
        }
        let mut visible = Capture::default();
        while visible.frames() < WINDOW {
            hang_tick!(format!(
                "Audio {backend:?} {mode:?} sought={sought} frames={}",
                visible.frames()
            ));
            let outcome = audio.next_chunk().expect("original or sought Audio PCM");
            if sought {
                while let Ok(envelope) = audio_events.try_recv() {
                    hang_tick!(format!(
                        "Audio {backend:?} {mode:?} sought={sought} frames={}",
                        visible.frames()
                    ));
                    match envelope.event {
                        AudioEvent::SeekLifecycle { stage, .. } => stages.push(stage),
                        AudioEvent::SeekComplete { position } => {
                            let ChunkOutcome::Chunk(chunk) = &outcome else {
                                panic!("seek completion requires committed PCM");
                            };
                            assert_eq!(
                                visible.frames(),
                                0,
                                "only the first sought PCM can complete the seek"
                            );
                            assert_eq!(position, chunk.meta.timestamp);
                            completions.push(position);
                        }
                        AudioEvent::SeekRejected { target } => {
                            panic!("seek to {target:?} was rejected");
                        }
                        _ => {}
                    }
                }
            }
            match outcome {
                ChunkOutcome::Chunk(chunk) => {
                    if sought && visible.frames() == 0 {
                        assert_eq!(
                            stages,
                            [
                                SeekLifecycleStage::SeekRequest,
                                SeekLifecycleStage::SeekApplied,
                                SeekLifecycleStage::OutputCommitted,
                            ]
                        );
                        assert_eq!(completions, [chunk.meta.timestamp]);
                        let start = chunk.meta.source_span.map_or(chunk.meta.timestamp, |span| {
                            span.position_at(0).expect("mapped sought start")
                        });
                        let frame = chunk.spec().duration_for(1).expect("one source frame");
                        assert!(start + frame >= target);
                        assert!(start <= target + frame);
                    }
                    visible.push(&chunk);
                    hang_reset!();
                }
                ChunkOutcome::Pending { .. } => yield_now().await,
                ChunkOutcome::Eof { .. } => {
                    panic!("the diagnostic window must contain three seconds")
                }
            }
        }
        if sought {
            assert_eq!(
                stages,
                [
                    SeekLifecycleStage::SeekRequest,
                    SeekLifecycleStage::SeekApplied,
                    SeekLifecycleStage::OutputCommitted,
                ]
            );
            assert_eq!(completions.len(), 1);
        }
        visible.report(
            if sought {
                "Audio-sought"
            } else {
                "Audio-original"
            },
            backend,
            mode,
        );
        assert_eq!(audio.media_origin(), Some(media_origin));
        visible.assert_media_phase(!sought, media_origin, &score.0);
    }
}
