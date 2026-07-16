#![cfg(not(target_arch = "wasm32"))]

use std::{fs, iter::from_fn, mem::size_of, num::NonZeroU32, path::Path};

use kithara::{
    assets::StoreOptions,
    audio::{BeatGrid, ReadOutcome, TrackBeat, analysis::TrackAnalysis},
    events::{AudioEvent, Event, EventBus},
    platform::time::{Duration, sleep},
    play::{
        PlayError, PlaybackDirection, Resource, ResourceBlueprint, ResourceConfig, SessionBeat,
        Tempo, TrackBinding,
    },
};
use kithara_integration_tests::{
    SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper, TestTempDir, kithara, temp_dir,
    wav::create_wav_header,
};
use num_traits::ToPrimitive;

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const BLOCK_FRAMES: usize = 512;
const CHANNELS: u16 = 2;
const FIXTURE_FRAMES: usize = 176_400;
const SAMPLE_RATE: u32 = 44_100;
const SESSION_TEMPO: f64 = 120.0;
const TRACK_TEMPO: f64 = 100.0;

fn binding(session_anchor: SessionBeat) -> TrackBinding {
    let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
    let frames_per_beat = (60.0 * f64::from(SAMPLE_RATE) / TRACK_TEMPO)
        .round()
        .to_u64()
        .expect("track tempo resolves to a frame span");
    let markers = (0..=6).map(|beat| beat * frames_per_beat).collect();
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(TRACK_TEMPO, markers, vec![0], Vec::new())),
        None,
        u64::try_from(FIXTURE_FRAMES).expect("fixture frame count fits u64"),
        sample_rate,
    );
    TrackBinding::new(
        &analysis,
        sample_rate,
        session_anchor,
        TrackBeat::new(0.0).expect("finite track anchor"),
        PlaybackDirection::Forward,
    )
    .expect("fixture analysis can be bound")
}

fn write_rendered_wav(path: &Path, rendered: &[f32]) {
    let data_bytes = rendered.len() * size_of::<i16>();
    let mut wav = create_wav_header(SAMPLE_RATE, CHANNELS, Some(data_bytes));
    wav.reserve(data_bytes);
    for sample in rendered {
        let quantized = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX))
            .round()
            .to_i16()
            .expect("clamped sample fits i16");
        wav.extend_from_slice(&quantized.to_le_bytes());
    }
    fs::write(path, wav).expect("write offline elastic artifact");
}

fn positive_crossing_frequency(rendered: &[f32]) -> Option<f64> {
    let mut previous = 0.0_f32;
    let mut started = false;
    let mut first = None;
    let mut last = 0usize;
    let mut crossings = 0usize;
    for (frame, channels) in rendered.chunks_exact(usize::from(CHANNELS)).enumerate() {
        let sample = channels[0];
        if !started {
            if sample.abs() < 0.05 {
                continue;
            }
            started = true;
            previous = sample;
            continue;
        }
        if previous <= 0.0 && sample > 0.0 {
            first.get_or_insert(frame);
            last = frame;
            crossings += 1;
        }
        previous = sample;
    }
    let first = first?;
    let intervals = crossings.checked_sub(1)?.to_f64()?;
    let span = last.checked_sub(first)?.to_f64()?;
    (span > 0.0).then_some(intervals * f64::from(SAMPLE_RATE) / span)
}

#[kithara::test(tokio, flash(false))]
async fn bound_track_renders_elastic_audio_to_an_offline_wav(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let spec = SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Frames(FIXTURE_FRAMES),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bit_rate: None,
    };
    let url = server.sine(&spec, 440.0).await;
    let public_resource_bus = EventBus::default();
    let mut public_resource_events = public_resource_bus.subscribe();
    let mut config = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .store(StoreOptions::new(temp_dir.path()))
        .byte_pool(harness.player().byte_pool().clone())
        .pcm_pool(harness.player().pcm_pool().clone())
        .build();
    config.set_bus(public_resource_bus);
    config = harness.player().prepare_config(config);
    let mut resource = Resource::new(config).await.expect("open WAV resource");
    let _ = resource.preload().await;
    while public_resource_events.try_recv().is_ok() {}

    harness
        .player()
        .ensure_engine_started()
        .expect("offline engine starts");
    harness
        .player()
        .ensure_slot()
        .expect("offline player slot is allocated");
    harness
        .set_session_tempo(Tempo::new(SESSION_TEMPO).expect("valid session tempo"))
        .expect("session tempo accepted");
    let activation_beat = SessionBeat::new(
        BLOCK_FRAMES.to_f64().expect("block size fits f64") * SESSION_TEMPO
            / (f64::from(SAMPLE_RATE) * 60.0),
    )
    .expect("finite activation beat");
    harness
        .player()
        .insert_with_binding(resource, None, binding(activation_beat), None)
        .await
        .expect("bound fixture inserts after off-path preparation");
    let leaked_seek = from_fn(|| public_resource_events.try_recv().ok()).find(|event| {
        matches!(
            event.event,
            Event::Audio(
                AudioEvent::SeekLifecycle { .. }
                    | AudioEvent::SeekComplete { .. }
                    | AudioEvent::SeekRejected { .. }
            )
        )
    });
    assert!(
        leaked_seek.is_none(),
        "internal preparation must not publish seek events on the application resource bus: {leaked_seek:?}"
    );
    for _ in 0..64 {
        harness.player().set_rate(1.0);
    }
    assert!(matches!(
        harness.player().select_item(0, true),
        Err(PlayError::SlotChannelFull { .. })
    ));
    assert_eq!(harness.player().current_index(), 0);
    assert!(
        harness
            .render(BLOCK_FRAMES)
            .iter()
            .all(|sample| sample.abs() <= f32::EPSILON),
        "a rejected load must not publish audible state"
    );
    harness
        .player()
        .select_item(0, true)
        .expect("the restored bound fixture loads on retry");

    let position_before_seek = harness.player().position_seconds();
    assert!(matches!(
        harness.player().seek_seconds(2.0),
        Err(PlayError::BoundTrackSeekRequiresSessionTransport)
    ));
    assert_eq!(harness.player().position_seconds(), position_before_seek);

    let mut rendered = Vec::with_capacity(BLOCK_FRAMES * 2 * 24);
    let mut events = Vec::new();
    let mut block_peaks = Vec::new();
    for _ in 0..24 {
        let block = harness.render(BLOCK_FRAMES);
        block_peaks.push(
            block
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs())),
        );
        rendered.extend(block);
        events.extend(harness.tick_and_drain());
        sleep(Duration::from_millis(1)).await;
    }

    assert_eq!(rendered.len(), BLOCK_FRAMES * 2 * 24);
    assert!(
        rendered.iter().any(|sample| sample.abs() > 1.0e-4),
        "bound playback must come from the elastic source path; peaks={block_peaks:?}; events={events:?}"
    );
    let frequency = positive_crossing_frequency(&rendered)
        .expect("bound elastic output contains enough complete tone periods");
    assert!(
        (frequency - 440.0).abs() <= 12.0,
        "pitch-preserving elastic output must retain the 440 Hz source tone, got {frequency:.3} Hz"
    );
    let artifact = temp_dir.path().join("bound-elastic-offline.wav");
    write_rendered_wav(&artifact, &rendered);
    assert!(
        fs::metadata(&artifact)
            .expect("offline artifact metadata")
            .len()
            > 44,
        "offline WAV must contain audio data"
    );
}

#[kithara::test(tokio, flash(false))]
async fn resource_blueprint_opens_independent_preparation_readers(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SAMPLE_RATE,
    );
    let spec = SignalSpec {
        format: SignalFormat::Wav,
        length: SignalSpecLength::Frames(BLOCK_FRAMES * 8),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bit_rate: None,
    };
    let url = server.sine(&spec, 220.0).await;
    let config = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .store(StoreOptions::new(temp_dir.path()))
        .byte_pool(harness.player().byte_pool().clone())
        .pcm_pool(harness.player().pcm_pool().clone())
        .build();
    let blueprint = ResourceBlueprint::new(harness.player().prepare_config(config));
    let mut active = blueprint.open().await.expect("open active reader");
    let mut prepared = blueprint.open().await.expect("open preparation reader");
    active.preload().await.expect("preload active reader");
    prepared
        .preload()
        .await
        .expect("preload preparation reader");

    drop(active);
    let mut output = [0.0; BLOCK_FRAMES * CHANNELS as usize];
    let read = prepared.read(&mut output).expect("read preparation reader");
    assert!(matches!(read, ReadOutcome::Frames { .. }));
    assert!(output.iter().any(|sample| sample.abs() > 1.0e-4));
}
