use std::{io::Cursor, num::NonZeroU32};

use ::kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{
        Audio, AudioConfig, AudioDecoderConfig, AudioEvent, AudioSource, ChunkOutcome,
        DecoderBackend as ObservedBackend, DecoderChangeCause, DecoderEvent, SeekLifecycleStage,
        SeekOutcome,
    },
    decode::{DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, PcmOrigin},
    events::EventBus,
    file::{File, FileConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::task::{spawn_blocking, yield_now},
    },
    resampler::NoResamplerBackend,
    signal::AudioSpec,
    stream::mock::NoopWorkerWake,
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
};
use kithara_test_fixtures::SignalAsset;
use kithara_test_utils::kithara;

#[kithara::test(native, tokio, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara::hang_watchdog(ctx = String)]
#[case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH)]
#[case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH)]
#[case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH)]
#[case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH)]
#[case(SignalAsset::PROFILE_M4A_AAC_44100_2CH)]
#[case(SignalAsset::PROFILE_TAGGED_MP3_ID3)]
#[case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3)]
async fn initial_visible_duration_matches_the_built_ruler_before_eof(#[case] asset: SignalAsset) {
    let server = TestServerHelper::new().await;
    let url = server.signal(asset);
    let body = reqwest::get(url.clone())
        .await
        .expect("encoded fixture response")
        .error_for_status()
        .expect("encoded fixture status")
        .bytes()
        .await
        .expect("encoded fixture bytes");
    let pools = pools();
    let decoder_bytes = body.to_vec();
    let decoder_pools = pools.clone();
    let (raw_before, raw_after, profile_before, profile_after) = spawn_blocking(move || {
        let mut decoder = DecoderFactory::create_with_probe(
            Cursor::new(decoder_bytes),
            Some(asset.ext()),
            DecoderConfig::<NoResamplerBackend, TestPools>::builder()
                .pools(decoder_pools)
                .build(),
        )
        .expect("initial encoded decoder");
        let raw_before = decoder.duration();
        let profile_before = decoder.gapless_profile(None);
        let origin_before = decoder.initial_pcm_origin().map(PcmOrigin::position);
        let DecoderChunkOutcome::Chunk(first) = decoder.next_chunk().expect("original decoder PCM")
        else {
            panic!("in-memory encoded source must produce original PCM");
        };
        assert!(first.frames() > 0);
        assert_eq!(
            first.samples.len(),
            first.frames() * usize::from(first.spec().channels)
        );
        assert!(first.samples.iter().all(|sample| sample.is_finite()));
        let raw_after = decoder.duration();
        let profile_after = decoder.gapless_profile(None);
        let origin_after = decoder.initial_pcm_origin().map(PcmOrigin::position);
        tracing::info!(asset = asset.name(), ?raw_before, ?raw_after, ?profile_before, ?profile_after,
            ?origin_before, ?origin_after, first_meta = ?first.meta,
            first_frames = first.frames(), first_samples = ?&first.samples[..first.samples.len().min(8)],
            "initial encoded duration owner receipt");
        (raw_before, raw_after, profile_before, profile_after)
    })
    .await
    .expect("initial raw decoder task completes");

    #[cfg(not(target_os = "android"))]
    if asset == SignalAsset::PROFILE_M4A_AAC_44100_2CH {
        assert_complete_aac_presentation(&body, url.clone()).await;
    }

    let file = FileConfig::for_src(url.into())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file)
        .hint(asset.ext().to_owned())
        .build();
    let mut audio = Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
        .await
        .expect("audio owner");
    let prepared_length = audio.duration();
    let prepared_geometry = (
        audio.media_origin(),
        audio.media_end(),
        audio.visible_origin(),
    );
    if matches!(prepared_geometry, (Some(_), Some(_), None)) {
        assert_eq!(prepared_length, None);
    } else {
        assert!(
            prepared_length.is_some(),
            "known initial geometry must publish its visible duration before PCM"
        );
    }
    let visible = loop {
        hang_tick!(format!("{} first original Audio PCM", asset.name()));
        match audio.next_chunk().expect("visible original PCM") {
            ChunkOutcome::Chunk(chunk) => break chunk,
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => {
                panic!("first output cannot be EOF for the six-second source")
            }
        }
    };
    assert!(visible.frames() > 0);
    assert!(
        !visible.meta.end_of_track,
        "initial length cannot be an EOF refinement"
    );
    assert_eq!(
        visible.samples.len(),
        visible.frames() * usize::from(visible.spec().channels)
    );
    assert!(visible.samples.iter().all(|sample| sample.is_finite()));
    let initial_length = audio
        .duration()
        .expect("initial known visible duration before EOF");
    if let Some(prepared_length) = prepared_length {
        assert_eq!(
            initial_length, prepared_length,
            "first original PCM must retain the known initial duration"
        );
    }
    let initial_frames = AudioSpec::new(2, NonZeroU32::new(44_100).expect("comparison rate"))
        .frame_at(initial_length)
        .expect("visible duration fits the comparison clock");
    tracing::info!(
        asset = asset.name(),
        ?initial_length,
        initial_frames,
        "initial Audio duration receipt"
    );
    assert!(
        initial_frames.abs_diff(264_600) <= 2_048,
        "{} initial length {initial_frames} must match built six-second ruler 264600 before EOF; raw_before={raw_before:?}, raw_after={raw_after:?}, profile_before={profile_before:?}, profile_after={profile_after:?}",
        asset.name()
    );
    let playhead = audio.playhead_read();
    assert_eq!(visible.meta.visible_origin, Some(visible.meta.timestamp));
    assert_eq!(playhead.duration(), Some(initial_length));
    let target = visible.meta.timestamp + Duration::from_secs(5);
    let mut audio_events = audio.events::<AudioEvent>();
    let before_seek = audio.position();
    match AudioSource::seek(&mut audio, target)
        .expect("first nonzero raw seek after original output")
    {
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
            panic!("five-second seek must retain a playable suffix: {other:?}")
        }
    }
    let mut stages = Vec::new();
    let mut completions = Vec::new();
    let sought = loop {
        hang_tick!(format!("{} first sought Audio PCM", asset.name()));
        let outcome = audio.next_chunk().expect("visible sought PCM");
        while let Ok(envelope) = audio_events.try_recv() {
            hang_tick!(format!("{} first sought Audio PCM", asset.name()));
            match envelope.event {
                AudioEvent::SeekLifecycle { stage, .. } => stages.push(stage),
                AudioEvent::SeekComplete { position } => {
                    let ChunkOutcome::Chunk(chunk) = &outcome else {
                        panic!("seek completion requires committed PCM");
                    };
                    assert_eq!(position, chunk.meta.timestamp);
                    completions.push(position);
                }
                AudioEvent::SeekRejected { target } => panic!("seek to {target:?} was rejected"),
                _ => {}
            }
        }
        match outcome {
            ChunkOutcome::Chunk(chunk) => break chunk,
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => panic!("one-second tail cannot be empty"),
        }
    };
    assert_eq!(
        stages,
        [
            SeekLifecycleStage::SeekRequest,
            SeekLifecycleStage::SeekApplied,
            SeekLifecycleStage::OutputCommitted,
        ]
    );
    assert_eq!(completions, [sought.meta.timestamp]);
    let start = sought
        .meta
        .source_span
        .map_or(sought.meta.timestamp, |span| {
            span.position_at(0).expect("mapped sought start")
        });
    let frame = sought.spec().duration_for(1).expect("one source frame");
    assert!(start + frame >= target);
    assert!(start <= target + frame);
    assert_eq!(sought.meta.visible_origin, visible.meta.visible_origin);
    tracing::info!(asset = asset.name(), visible_meta = ?visible.meta, sought_meta = ?sought.meta,
        visible_frames = visible.frames(), visible_samples = ?&visible.samples[..visible.samples.len().min(8)],
        "original visible PCM receipt");
}

#[cfg(not(target_os = "android"))]
#[kithara::hang_watchdog]
async fn assert_complete_aac_presentation(bytes: &[u8], url: url::Url) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("AAC fixture rate"));
    let origin = spec.duration_for(1_024).expect("MP4 edit origin");
    let endpoint = Duration::from_secs(6);
    let reference = kithara_integration_tests::pcm_oracle::decode_native(bytes)
        .await
        .expect("independent native FFmpeg AAC decode");
    assert_eq!((reference.sample_rate, reference.channels), (44_100, 2));
    assert_eq!(reference.samples.len(), 264_600 * 2);

    let decoder_bytes = bytes.to_vec();
    let native = spawn_blocking(move || {
        let mut decoder = DecoderFactory::create_with_probe(
            Cursor::new(decoder_bytes),
            Some("m4a"),
            DecoderConfig::<NoResamplerBackend, TestPools>::builder()
                .backend(DecoderBackend::Symphonia)
                .pools(pools())
                .build(),
        )
        .expect("explicit software AAC owner");
        assert_eq!(decoder.media_origin(), Some(origin));
        assert_eq!(
            decoder.duration(),
            Some(::kithara::decode::DecoderDuration::MediaEnd(endpoint))
        );
        let mut native = Vec::new();
        let mut watchdog =
            kithara_test_utils::hang::HangDetector::<kithara_test_utils::hang::NoContext>::new(
                concat!(module_path!(), "::assert_complete_aac_presentation::native"),
                kithara_test_utils::hang::default_timeout(),
            );
        loop {
            watchdog.tick_from(file!(), line!());
            match decoder.next_chunk().expect("actual AAC native PCM") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    assert_eq!(
                        spec.frame_at(chunk.meta.timestamp)
                            .expect("native PCM coordinate"),
                        (native.len() / 2) as u64
                    );
                    assert_eq!(chunk.meta.media_origin, Some(origin));
                    assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
                    native.extend_from_slice(&chunk.samples);
                    watchdog.reset_from(file!(), line!());
                }
                DecoderChunkOutcome::Eof => break,
                other @ DecoderChunkOutcome::Pending(_) => {
                    panic!("in-memory AAC cannot wait for source data: {other:?}")
                }
            }
        }
        native
    })
    .await
    .expect("native AAC decoder task completes");
    assert_eq!(
        native.len(),
        265_624 * 2,
        "actual packet duration must exclude block padding"
    );
    let expected = &native[1_024 * 2..];
    for start in [0, 132_300 - 4_096, 264_600 - 8_192] {
        let range = start * 2..(start + 8_192) * 2;
        let (mut dot, mut actual_energy, mut reference_energy) = (0.0_f64, 0.0_f64, 0.0_f64);
        for (actual, reference) in expected[range.clone()]
            .iter()
            .zip(&reference.samples[range])
        {
            let (actual, reference) = (f64::from(*actual), f64::from(*reference));
            dot += actual * reference;
            actual_energy += actual * actual;
            reference_energy += reference * reference;
        }
        let correlation = dot / (actual_energy * reference_energy).sqrt();
        assert!(
            correlation >= 0.999,
            "fixed source window at{start} lost AAC content: {correlation}"
        );
    }

    let pool = pools();
    let file = FileConfig::for_src(url.into())
        .store(
            AssetStore::builder(pool.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pool.clone())
        .build();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file)
        .hint("m4a".to_owned())
        .decoder(
            AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .build(),
        )
        .build();
    let mut audio = Audio::prepare(config, Arc::new(NoopWorkerWake), pool)
        .await
        .expect("visible AAC owner");
    assert_eq!(audio.media_origin(), Some(origin));
    assert_eq!(audio.media_end(), Some(endpoint));
    assert_eq!(audio.visible_origin(), None);
    assert_eq!(audio.duration(), None);
    let mut visible = Vec::new();
    loop {
        hang_tick!();
        match audio.next_chunk().expect("visible AAC PCM") {
            ChunkOutcome::Chunk(chunk) => {
                if visible.is_empty() {
                    assert!(!chunk.meta.end_of_track, "initial duration precedes EOF");
                    assert_eq!(audio.visible_origin(), Some(origin));
                    assert_eq!(audio.duration(), Some(endpoint));
                }
                assert_eq!(
                    spec.frame_at(chunk.meta.timestamp)
                        .expect("visible native coordinate"),
                    1_024 + (visible.len() / 2) as u64
                );
                assert_eq!(chunk.meta.media_origin, Some(origin));
                assert_eq!(chunk.meta.media_end, Some(endpoint));
                visible.extend_from_slice(&chunk.samples);
                hang_reset!();
            }
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => break,
        }
    }
    assert_eq!(
        visible.len(),
        264_600 * 2,
        "EOF must retain the complete visible AAC tail"
    );
    assert_eq!(
        visible, expected,
        "routing must preserve every independently decoded source frame"
    );
    assert_eq!(audio.media_end(), Some(endpoint));
    assert_eq!(audio.duration(), Some(endpoint));
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara::hang_watchdog(ctx = String)]
#[case::mp3(
    SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH,
    DecoderBackend::default()
)]
#[case::aac(SignalAsset::PROFILE_M4A_AAC_44100_2CH, DecoderBackend::default())]
#[case::symphonia_mp3(
    SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH,
    DecoderBackend::Symphonia
)]
#[case::symphonia_aac(SignalAsset::PROFILE_M4A_AAC_44100_2CH, DecoderBackend::Symphonia)]
async fn preloaded_original_pcm_preserves_its_origin_through_the_first_raw_seek(
    #[case] asset: SignalAsset,
    #[case] backend: DecoderBackend,
) {
    let server = TestServerHelper::new().await;
    let pools = pools();
    let file = FileConfig::for_src(server.signal(asset).into())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let bus = EventBus::default();
    let mut decoder_events = bus.subscribe::<DecoderEvent>();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file)
        .hint(asset.ext().to_owned())
        .events(bus)
        .decoder(AudioDecoderConfig::builder().backend(backend).build())
        .build();
    let mut audio = Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
        .await
        .expect("source owner");
    let DecoderEvent::DecoderChanged {
        backend: actual,
        cause: DecoderChangeCause::Initial,
        ..
    } = decoder_events
        .try_recv()
        .expect("initial decoder receipt")
        .event
    else {
        panic!("initial decoder must report its backend");
    };
    if matches!(backend, DecoderBackend::Symphonia) {
        assert_eq!(actual, ObservedBackend::Symphonia);
    }
    let mut audio_events = audio.events::<AudioEvent>();
    let reference_file = FileConfig::for_src(server.signal(asset).into())
        .store(
            AssetStore::builder(kithara_integration_tests::bufpool_ext::pools())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(kithara_integration_tests::bufpool_ext::pools())
        .build();
    let reference_config =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(reference_file)
            .hint(asset.ext().to_owned())
            .decoder(AudioDecoderConfig::builder().backend(backend).build())
            .build();
    let mut reference = Audio::prepare(
        reference_config,
        Arc::new(NoopWorkerWake),
        kithara_integration_tests::bufpool_ext::pools(),
    )
    .await
    .expect("independent original source");
    let origin = loop {
        hang_tick!(format!("{} reference original PCM", asset.name()));
        match reference.next_chunk().expect("reference original PCM") {
            ChunkOutcome::Chunk(chunk) => {
                break chunk.meta.visible_origin.expect("original geometry");
            }
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => panic!("six-second reference is not empty"),
        }
    };
    loop {
        hang_tick!(format!("{} original preload", asset.name()));
        audio.preload().expect("preload the real source");
        if audio_events
            .try_recv()
            .is_ok_and(|event| matches!(event.event, AudioEvent::FormatDetected { .. }))
        {
            hang_reset!();
            break;
        }
        yield_now().await;
    }
    assert!(audio.is_preloaded());
    let mut audio_events = audio.events::<AudioEvent>();
    let target = origin + Duration::from_secs(1);
    let before_seek = audio.position();
    match AudioSource::seek(&mut audio, target).expect("first raw seek") {
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
            panic!("one-second seek must retain a playable suffix: {other:?}")
        }
    }
    let mut stages = Vec::new();
    let mut completions = Vec::new();
    let mut served = 0_u64;
    let mut endpoint = None;
    loop {
        hang_tick!(format!("{} sought PCM frames={served}", asset.name()));
        let outcome = audio.next_chunk().expect("real sought PCM");
        while let Ok(envelope) = audio_events.try_recv() {
            hang_tick!(format!("{} sought PCM frames={served}", asset.name()));
            match envelope.event {
                AudioEvent::SeekLifecycle { stage, .. } => stages.push(stage),
                AudioEvent::SeekComplete { position } => {
                    let ChunkOutcome::Chunk(chunk) = &outcome else {
                        panic!("seek completion requires committed PCM");
                    };
                    assert_eq!(served, 0, "only the first sought PCM can complete the seek");
                    assert_eq!(position, chunk.meta.timestamp);
                    completions.push(position);
                }
                AudioEvent::SeekRejected { target } => panic!("seek to {target:?} was rejected"),
                _ => {}
            }
        }
        match outcome {
            ChunkOutcome::Chunk(chunk) => {
                if served < 4096 {
                    eprintln!(
                        "SOUGHT_PCM asset={} served={served} origin={origin:?} target={target:?} prior={endpoint:?} meta={:?}",
                        asset.name(),
                        chunk.meta,
                    );
                }
                assert_eq!(
                    chunk.samples.len(),
                    chunk.frames() * usize::from(chunk.spec().channels)
                );
                assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
                assert_eq!(chunk.meta.visible_origin, Some(origin));
                let start = chunk.meta.source_span.map_or(chunk.meta.timestamp, |span| {
                    span.position_at(0).expect("mapped start")
                });
                if let Some(previous) = endpoint {
                    assert_eq!(start, previous);
                } else {
                    let frame = chunk.spec().duration_for(1).expect("one source frame");
                    assert!(start + frame >= target);
                    assert!(start <= target + frame);
                    assert_eq!(
                        stages,
                        [
                            SeekLifecycleStage::SeekRequest,
                            SeekLifecycleStage::SeekApplied,
                            SeekLifecycleStage::OutputCommitted,
                        ]
                    );
                    assert_eq!(completions, [chunk.meta.timestamp]);
                }
                endpoint = Some(
                    chunk
                        .meta
                        .source_span
                        .map_or(chunk.meta.end_timestamp, |span| {
                            span.position_at(u64::from(chunk.meta.frames))
                                .expect("mapped endpoint")
                        }),
                );
                served += u64::from(chunk.meta.frames);
                hang_reset!();
            }
            ChunkOutcome::Pending { .. } => {}
            ChunkOutcome::Eof { .. } => break,
        }
        yield_now().await;
    }
    assert!(served > 0);
    assert_eq!(
        stages,
        [
            SeekLifecycleStage::SeekRequest,
            SeekLifecycleStage::SeekApplied,
            SeekLifecycleStage::OutputCommitted,
        ]
    );
    assert_eq!(completions.len(), 1);
    let endpoint = endpoint.expect("sought suffix emitted PCM");
    assert_eq!(audio.duration(), Some(endpoint - origin));
}
