#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    audio::{
        AudioConfig, AudioControl, AudioRead, AudioSession, ChunkOutcome, DecoderChangeCause,
        DecoderEvent, ReadOutcome, RubatoBackend,
    },
    platform::time::{self, Duration, Instant},
    play::{PlayWorker, PlayWorkerConfig, TrackConfig},
    signal::{AudioChunk, SegmentId},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
    warp::{StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    bufpool_ext::{TestPools, pools},
    event::TestEvent,
    kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
    mock::LaneAudio,
    reads::{blocking_audio, read_to_eof, read_until_samples},
};
use kithara_test_fixtures::integration_fixtures::{
    audio_wav_8000, audio_wav_44100, audio_wav_132300, audio_wav_176400, audio_wav_1323000,
};

fn wav_stream(wav: &[u8]) -> AudioConfig<MemStream> {
    let source = MemorySource::new(wav.to_vec());
    let stream = MemStreamConfig {
        source: Some(source),
        event_bus: None,
    };
    AudioConfig::<MemStream>::for_stream(stream)
        .hint("wav".to_string())
        .build()
}

async fn wait_for_chunk(
    mut audio: LaneAudio<Stream<MemStream>, TestPools>,
    budget: Duration,
) -> (LaneAudio<Stream<MemStream>, TestPools>, AudioChunk) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (next_audio, outcome) = blocking_audio(audio, AudioRead::next_chunk).await;
        audio = next_audio;
        match outcome.expect("decode while waiting for a PCM chunk") {
            ChunkOutcome::Chunk(chunk) => return (audio, *chunk),
            ChunkOutcome::Pending { .. } => time::sleep(Duration::from_millis(10)).await,
            ChunkOutcome::Eof { .. } => panic!("source reached EOF while waiting for PCM"),
        }
    }
    panic!("timed out waiting for ChunkOutcome::Chunk");
}

/// One read pass with no budget of its own — the caller owns the deadline.
/// Unlike [`wait_for_frames`], a pass that yields no frames is a result, not a
/// panic, so a caller can keep the output moving while it watches the bus.
async fn pump_once(
    audio: LaneAudio<Stream<MemStream>, TestPools>,
) -> (LaneAudio<Stream<MemStream>, TestPools>, ReadOutcome) {
    let mut buf = [0.0f32; 256];
    let (audio, (_buf, outcome)) = blocking_audio(audio, move |audio| {
        let outcome = audio.read(&mut buf);
        (buf, outcome)
    })
    .await;
    (audio, outcome.expect("read"))
}

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn basic_decode_to_eof(audio_wav_8000: &'static [u8]) {
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let config = wav_stream(audio_wav_8000);
    let audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("audio construction");

    let (_audio, frames) = blocking_audio(audio, read_to_eof).await;
    assert!(
        frames >= 8_000,
        "expected at least the input frame count, got {frames}"
    );
}

/// A route change resumes from admitted Warp progress, not the consumer head.
///
/// The head this is measured against is read immediately before the route is
/// selected, because that is the moment the property is about. Read after the
/// switch it also carries whatever the switch handed the reader, so the head
/// climbs towards the resume point and the comparison ends up between a number
/// and itself. The read that follows the switch stays and its chunk is now
/// examined rather than dropped: an off-thread consumer wakes the worker by
/// reading, so the rebuild needs that read to make progress at all, and it can
/// be the chunk the rebuild lands in.
#[kithara::test(tokio, timeout(Duration::from_secs(15)), hang_timeout_secs(5))]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
async fn non_unity_route_change_resumes_ahead_of_the_consumer(
    audio_wav_1323000: &'static [u8],
    #[case] backend: StretchKind,
) {
    const PRELOAD_CHUNKS: usize = 32;
    const RING_CHUNKS: usize = 48;
    const SOURCE_RATE: u32 = 44_100;
    const TARGET_RATE: u32 = 48_000;

    let source_rate = NonZeroU32::new(SOURCE_RATE).expect("source rate is non-zero");
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("target rate is non-zero");
    let wav = audio_wav_1323000.to_vec();
    let stream = MemStreamConfig {
        source: Some(MemorySource::new(wav)),
        event_bus: None,
    };
    let audio = AudioConfig::<MemStream, RubatoBackend>::for_stream(stream)
        .media_info(
            MediaInfo::builder()
                .channels(2)
                .codec(AudioCodec::Pcm)
                .container(ContainerFormat::Wav)
                .sample_rate(SOURCE_RATE)
                .build(),
        )
        .host_sample_rate(source_rate)
        .hint("wav".to_owned())
        .build();
    let config = TrackConfig::for_audio(audio)
        .preload_chunks(NonZeroUsize::new(PRELOAD_CHUNKS).expect("preload count is non-zero"))
        .audio_buffer_chunks(NonZeroUsize::new(RING_CHUNKS).expect("ring count is non-zero"))
        .warp(
            WarpConfig::builder()
                .speed(0.5)
                .backend(backend)
                .keylock(true)
                .build(),
        )
        .build();
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("audio construction");
    let mut events = audio.event_bus().subscribe();
    let mut audio = audio;
    kithara_integration_tests::mock::wait_for_preload(&mut audio, "non-unity source").await;

    let (mut audio, first) = wait_for_chunk(audio, Duration::from_secs(2)).await;
    let resume_margin = first
        .meta
        .end_timestamp
        .saturating_sub(first.meta.timestamp);
    assert!(
        !resume_margin.is_zero(),
        "source chunk span must be non-zero"
    );
    let committed = audio.position();
    let decoded_frontier = audio.decoded_frontier();
    assert!(
        decoded_frontier < audio.duration().expect("finite WAV duration"),
        "route change must happen before the preloaded source reaches EOF"
    );
    let admitted_lead = decoded_frontier.saturating_sub(committed);
    assert!(
        admitted_lead > Duration::from_millis(250),
        "fixture needs admitted PCM well ahead of the consumer; \
         decoded_frontier={decoded_frontier:?}, committed={committed:?}, \
         lead={admitted_lead:?}"
    );

    let committed_at_route = audio.position();
    audio.set_host_sample_rate(target_rate);
    let (mut audio, candidate) = wait_for_chunk(audio, Duration::from_secs(2)).await;

    loop {
        let envelope = events.recv().await.expect("decoder event bus remains open");
        if matches!(
            envelope.event,
            TestEvent::Decoder(DecoderEvent::DecoderChanged {
                cause: DecoderChangeCause::HostRateChange,
                ..
            })
        ) {
            break;
        }
    }

    let rebuilt = if candidate.meta.spec.sample_rate == target_rate {
        candidate
    } else {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for rebuilt PCM");
            let (next_audio, chunk) = wait_for_chunk(audio, remaining).await;
            audio = next_audio;
            if chunk.meta.spec.sample_rate == target_rate {
                break chunk;
            }
        }
    };
    assert!(
        rebuilt.meta.timestamp >= committed_at_route.saturating_add(resume_margin),
        "route recreation must resume from admitted Warp progress, not the \
         consumer head; rebuilt={:?}, committed_at_route={committed_at_route:?}, \
         resume_margin={resume_margin:?}",
        rebuilt.meta.timestamp
    );
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(10)),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
async fn seek_during_active_decode_completes_without_hang(audio_wav_132300: &'static [u8]) {
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let config = wav_stream(audio_wav_132300);
    let audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("audio construction");
    let (audio, _initial_frames) =
        blocking_audio(audio, |audio| read_until_samples(audio, 1)).await;
    let (mut audio, seek_result) =
        blocking_audio(audio, |audio| audio.seek(Duration::from_secs_f64(1.5))).await;
    seek_result.expect("seek");
    let expected_segment = audio.segment();
    kithara_integration_tests::mock::wait_for_preload(&mut audio, "active decode seek").await;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_complete = false;
    while Instant::now() < deadline && !saw_complete {
        let (next_audio, outcome) = pump_once(audio).await;
        audio = next_audio;
        if matches!(outcome, ReadOutcome::Pending { .. }) {
            time::sleep(Duration::from_millis(20)).await;
        }
        saw_complete = audio.committed_segment() == Some(expected_segment);
    }
    assert!(
        saw_complete,
        "matching committed segment must arrive after seek"
    );
    let (_audio, frames_after) = blocking_audio(audio, |audio| read_until_samples(audio, 1)).await;
    assert!(
        frames_after > 0,
        "audio must keep producing frames after seek"
    );
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(15)),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
async fn rapid_seeks_via_timeline_all_complete(audio_wav_176400: &'static [u8]) {
    const SEEK_COUNT: usize = 6;

    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let config = wav_stream(audio_wav_176400);
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("audio construction");
    let (next_audio, _) = blocking_audio(audio, |audio| read_until_samples(audio, 1)).await;
    audio = next_audio;
    let mut expected_segments = Vec::with_capacity(SEEK_COUNT);
    let mut committed = Vec::new();
    let mut turns = 0usize;
    for index in 0..SEEK_COUNT {
        let target = Duration::from_millis(200 + (index as u64) * 250);
        let (next_audio, seek_result) =
            blocking_audio(audio, move |audio| audio.seek(target)).await;
        audio = next_audio;
        seek_result.expect("seek");
        let expected = audio.segment();
        expected_segments.push(expected);
        kithara_integration_tests::mock::wait_for_preload(&mut audio, "rapid seek").await;
        let (next_audio, _) = blocking_audio(audio, |audio| read_until_samples(audio, 1)).await;
        audio = next_audio;
        turns += 1;
        let observed = audio
            .committed_segment()
            .expect("seek produced committed PCM");
        assert_eq!(observed, expected, "each seek commits only its own segment");
        committed.push(observed);
    }
    let highest_expected = *expected_segments
        .iter()
        .max()
        .expect("at least one segment");
    let last_complete: Option<SegmentId> = audio.committed_segment();
    assert_eq!(
        last_complete,
        Some(highest_expected),
        "last committed segment must match the highest requested segment; requested {expected_segments:?}, output-committed {committed:?}, {turns} read turn(s)"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn truncated_wav_surfaces_decode_error_or_eof(audio_wav_44100: &'static [u8]) {
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let mut wav = audio_wav_44100.to_vec();
    wav.truncate(wav.len() / 4);
    let source = MemorySource::new(wav);
    let config = AudioConfig::<MemStream>::for_stream(MemStreamConfig {
        source: Some(source),
        event_bus: None,
    })
    .hint("wav".to_string())
    .build();

    let audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("audio construction");

    let (_audio, saw_terminal) = blocking_audio(audio, |audio| {
        let mut buf = [0.0f32; 4096];
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Eof { .. }) | Err(_) => return true,
                Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
            }
        }
        false
    })
    .await;
    assert!(
        saw_terminal,
        "truncated WAV must surface either Eof or DecodeError"
    );
}
