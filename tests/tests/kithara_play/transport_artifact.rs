#![cfg(not(target_arch = "wasm32"))]

use std::{fs, mem::size_of, num::NonZeroU32, ops::Range, path::Path};

use kithara::{
    audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis},
    platform::{
        sync::Arc,
        time::{Duration, sleep},
    },
    play::{
        PlaybackDirection, PlayerEvent, Resource, ResourceConfig, SessionBeat, SessionTrackControl,
        SessionTransportSnapshot, Tempo, TrackBinding,
    },
};
use kithara_integration_tests::{
    TestTempDir, kithara, offline::OfflineSession, temp_dir, wav::create_wav_header,
};
use num_traits::ToPrimitive;

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const BLOCK_FRAMES: usize = 512;
const CHANNELS: u16 = 2;
const MARKER_BEATS: usize = 8;
const SAMPLE_RATE: u32 = 44_100;
const SOURCE_TEMPO: u32 = 120;

fn frames_per_beat(tempo: u32) -> usize {
    usize::try_from(60 * SAMPLE_RATE / tempo).expect("beat span fits usize")
}

fn marker_source_wav() -> Vec<u8> {
    const MARKERS: [f32; MARKER_BEATS] = [-0.7, -0.5, -0.3, -0.1, 0.1, 0.3, 0.5, 0.7];
    let source_frames = frames_per_beat(SOURCE_TEMPO) * MARKER_BEATS;
    let data_bytes = source_frames * usize::from(CHANNELS) * size_of::<i16>();
    let mut wav = create_wav_header(SAMPLE_RATE, CHANNELS, Some(data_bytes));
    wav.reserve(data_bytes);
    for frame in 0..source_frames {
        let sample = MARKERS[frame / frames_per_beat(SOURCE_TEMPO)];
        let quantized = (sample * f32::from(i16::MAX))
            .round()
            .to_i16()
            .expect("marker sample fits i16");
        for _ in 0..CHANNELS {
            wav.extend_from_slice(&quantized.to_le_bytes());
        }
    }
    wav
}

fn binding(
    session_anchor: SessionBeat,
    track_anchor: TrackBeat,
    direction: PlaybackDirection,
) -> TrackBinding {
    let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate");
    let beat_frames = u64::try_from(frames_per_beat(SOURCE_TEMPO)).expect("beat span fits u64");
    let source_frames = beat_frames * u64::try_from(MARKER_BEATS).expect("marker count fits u64");
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(
            f64::from(SOURCE_TEMPO),
            (0..=MARKER_BEATS)
                .map(|beat| u64::try_from(beat).expect("marker index fits u64") * beat_frames)
                .collect(),
            vec![0],
            Vec::new(),
        )),
        None,
        source_frames,
        sample_rate,
    );
    TrackBinding::new(
        &analysis,
        sample_rate,
        session_anchor,
        track_anchor,
        direction,
    )
    .expect("marker source can be bound")
}

async fn marker_resource(harness: &OfflinePlayerHarness, source: &Path) -> Resource {
    let config = ResourceConfig::for_src(source.to_string_lossy())
        .expect("valid marker fixture path")
        .byte_pool(harness.player().byte_pool().clone())
        .pcm_pool(harness.player().pcm_pool().clone())
        .build();
    let mut resource = Resource::new(harness.player().prepare_config(config))
        .await
        .expect("open marker fixture");
    resource.preload().await.expect("preload marker fixture");
    resource
}

async fn render_exact(
    harnesses: &[OfflinePlayerHarness],
    phase: &str,
    mut frames: usize,
) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(frames * usize::from(CHANNELS));
    while frames > 0 {
        let block_frames = frames.min(BLOCK_FRAMES);
        rendered.extend(harnesses[0].render(block_frames));
        drain_players(harnesses, phase, rendered.len() / usize::from(CHANNELS));
        frames -= block_frames;
        sleep(Duration::from_secs_f64(
            block_frames.to_f64().expect("block size fits f64") / f64::from(SAMPLE_RATE),
        ))
        .await;
    }
    rendered
}

fn drain_players(harnesses: &[OfflinePlayerHarness], phase: &str, rendered_frames: usize) {
    for (index, harness) in harnesses.iter().enumerate() {
        let events = harness.tick_and_drain();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, PlayerEvent::ItemDidFail { .. })),
            "bound transport track {index} failed during {phase} at frame {rendered_frames}: {events:?}"
        );
    }
}

fn shared_transport(harnesses: &[OfflinePlayerHarness]) -> SessionTransportSnapshot {
    let snapshot = harnesses[0]
        .session_transport()
        .expect("session transport is observable");
    assert!(harnesses.iter().all(|harness| {
        harness
            .session_transport()
            .is_ok_and(|observed| observed == snapshot)
    }));
    snapshot
}

fn commit_tempo(harnesses: &[OfflinePlayerHarness], tempo: Tempo) {
    let before = shared_transport(harnesses);
    let peers: Vec<_> = harnesses[1..]
        .iter()
        .map(|harness| harness.player().as_ref())
        .collect();
    harnesses[0]
        .player()
        .set_session_tempo(&peers, tempo)
        .expect("multi-track tempo change is accepted");

    let committed = (0..BLOCK_FRAMES * 8)
        .find_map(|frame| {
            let _ = harnesses[0].render(1);
            drain_players(harnesses, "tempo commit", frame + 1);
            harnesses[0]
                .tick_session()
                .expect("tempo transaction progresses");
            let snapshot = shared_transport(harnesses);
            (snapshot.revision() != before.revision()).then_some(snapshot)
        })
        .expect("tempo commits within the render budget");
    assert_eq!(committed.revision(), before.revision() + 1);
    assert_eq!(committed.tempo(), tempo);
}

async fn commit_seek(harnesses: &[OfflinePlayerHarness], target: SessionBeat) {
    let before = shared_transport(harnesses);
    let players: Vec<_> = harnesses
        .iter()
        .map(|harness| Arc::clone(harness.player()))
        .collect();
    let seek = kithara::platform::tokio::task::spawn(async move {
        let peers: Vec<_> = players[1..].iter().map(AsRef::as_ref).collect();
        players[0].seek_session(&peers, target).await
    });

    for frame in 0..512 {
        if seek.is_finished() {
            break;
        }
        let _ = harnesses[0].render(1);
        drain_players(harnesses, "seek preparation", frame + 1);
        sleep(Duration::from_millis(10)).await;
    }
    assert!(seek.is_finished(), "session seek preparation stays bounded");
    seek.await
        .expect("session seek task joins")
        .expect("session seek preparation succeeds");

    let committed = (0..BLOCK_FRAMES * 8)
        .find_map(|frame| {
            let _ = harnesses[0].render(1);
            drain_players(harnesses, "seek commit", frame + 1);
            harnesses[0]
                .tick_session()
                .expect("seek transaction progresses");
            let snapshot = shared_transport(harnesses);
            (snapshot.revision() != before.revision()).then_some(snapshot)
        })
        .expect("seek commits within the render budget");
    let one_frame = committed.tempo().beats_per_minute() / (f64::from(SAMPLE_RATE) * 60.0);
    assert_eq!(committed.revision(), before.revision() + 1);
    assert!((committed.position().get() - target.get() - one_frame).abs() <= one_frame);
}

async fn render_transport_artifact(source: &Path) -> Vec<f32> {
    let session = Arc::new(OfflineSession::new_manual());
    let harnesses: Vec<_> = (0..2)
        .map(|_| {
            OfflinePlayerHarness::in_session(
                OfflinePlayerOptions::builder()
                    .crossfade_duration(0.0)
                    .build(),
                SAMPLE_RATE,
                Arc::clone(&session),
            )
        })
        .collect();
    for (harness, volume) in harnesses.iter().zip([0.25, 0.75]) {
        harness
            .player()
            .ensure_engine_started()
            .expect("offline engine starts");
        harness
            .player()
            .ensure_slot()
            .expect("offline slot is allocated");
        harness.player().set_volume(volume);
    }
    harnesses[0]
        .set_session_tempo(Tempo::new(120.0).expect("valid initial tempo"))
        .expect("initial tempo is accepted");
    let _ = harnesses[0].render(1);
    let anchor = shared_transport(&harnesses).position();

    let forward = marker_resource(&harnesses[0], source).await;
    let reverse = marker_resource(&harnesses[1], source).await;
    harnesses[0]
        .player()
        .insert_with_binding(
            forward,
            None,
            binding(
                anchor,
                TrackBeat::new(0.0).expect("finite forward anchor"),
                PlaybackDirection::Forward,
            ),
            None,
        )
        .await
        .expect("forward marker track prepares");
    harnesses[1]
        .player()
        .insert_with_binding(
            reverse,
            None,
            binding(
                anchor,
                TrackBeat::new(6.0).expect("finite reverse anchor"),
                PlaybackDirection::Reverse,
            ),
            None,
        )
        .await
        .expect("reverse marker track prepares");
    for harness in &harnesses {
        harness
            .player()
            .select_item(0, true)
            .expect("prepared marker track starts");
    }

    let mut rendered = render_exact(&harnesses, "initial tempo", frames_per_beat(120) * 3).await;
    commit_tempo(&harnesses, Tempo::new(100.0).expect("valid changed tempo"));
    commit_seek(
        &harnesses,
        SessionBeat::new(1.25).expect("finite seek target"),
    )
    .await;
    rendered.extend(
        render_exact(
            &harnesses,
            "changed tempo after seek",
            frames_per_beat(100) * 3,
        )
        .await,
    );
    let playing: Vec<_> = harnesses
        .iter()
        .map(|harness| harness.player().is_playing())
        .collect();
    assert!(
        playing.iter().all(|playing| *playing),
        "players stopped: {playing:?}"
    );
    rendered
}

fn channel_mean(rendered: &[f32], frames: Range<usize>) -> f32 {
    let channels = usize::from(CHANNELS);
    let samples = rendered[frames.start * channels..frames.end * channels]
        .chunks_exact(channels)
        .map(|frame| frame[0]);
    let (sum, count) = samples.fold((0.0_f32, 0usize), |(sum, count), sample| {
        (sum + sample, count + 1)
    });
    sum / count.to_f32().expect("marker window is non-empty")
}

fn beat_means(rendered: &[f32], beat_frames: usize) -> Vec<f32> {
    (0..3)
        .map(|beat| {
            let start = beat * beat_frames + beat_frames / 4;
            let end = beat * beat_frames + beat_frames * 3 / 4;
            channel_mean(rendered, start..end)
        })
        .collect()
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
    fs::write(path, wav).expect("write transport acceptance artifact");
}

#[kithara::test(tokio, flash(false))]
async fn multi_track_transport_writes_deterministic_tempo_seek_reverse_wav(temp_dir: TestTempDir) {
    let source = temp_dir.path().join("transport-markers.wav");
    fs::write(&source, marker_source_wav()).expect("write marker fixture");

    let first = render_transport_artifact(&source).await;
    let second = render_transport_artifact(&source).await;
    assert_eq!(first, second, "offline transport PCM must be bit-exact");
    assert!(first.iter().all(|sample| sample.is_finite()));
    assert!(first.iter().any(|sample| sample.abs() > 0.01));

    let initial_frames = frames_per_beat(120) * 3;
    let initial_samples = initial_frames * usize::from(CHANNELS);
    let initial = beat_means(&first[..initial_samples], frames_per_beat(120));
    let seeked = beat_means(&first[initial_samples..], frames_per_beat(100));
    assert!(initial.windows(2).all(|pair| pair[0] > pair[1]));
    assert!(seeked.windows(2).all(|pair| pair[0] > pair[1]));
    assert!((seeked[0] - initial[1]).abs() <= 0.02);
    assert!((seeked[1] - initial[2]).abs() <= 0.02);

    let artifact = temp_dir.path().join("transport-acceptance.wav");
    write_rendered_wav(&artifact, &first);
    assert_eq!(
        fs::metadata(&artifact)
            .expect("transport artifact metadata")
            .len(),
        u64::try_from(44 + first.len() * size_of::<i16>()).expect("artifact size fits u64")
    );
}
