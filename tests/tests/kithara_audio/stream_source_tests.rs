#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    audio::{
        AudioConfig, AudioControl, AudioRead, AudioSession, ChunkOutcome, ConsumerWakeMode,
        ReadOutcome, RubatoBackend,
    },
    events::{AudioEvent, DecoderChangeCause, DecoderEvent, Event, SeekEpoch, SeekLifecycleStage},
    platform::time::{self, Duration, Instant},
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio, TrackConfig},
    signal::AudioChunk,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
    warp::{StretchControls, StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    bufpool_ext::{TestPools, pools},
    kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
    reads::blocking_audio,
};
use kithara_test_fixtures::signal;

fn wav_stream(samples: usize) -> AudioConfig<MemStream> {
    let wav = signal::wav(44_100, 2, samples, signal::TONE);
    let source = MemorySource::new(wav);
    let stream = MemStreamConfig {
        source: Some(source),
        event_bus: None,
    };
    AudioConfig::<MemStream>::for_stream(stream)
        .hint("wav".to_string())
        .build()
}

async fn wait_for_frames(
    mut audio: RegisteredAudio<Stream<MemStream>, TestPools>,
    budget: Duration,
) -> (RegisteredAudio<Stream<MemStream>, TestPools>, usize) {
    let mut buf = [0.0f32; 256];
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (next_audio, (next_buf, outcome)) = blocking_audio(audio, move |audio| {
            let outcome = audio.read(&mut buf);
            (buf, outcome)
        })
        .await;
        audio = next_audio;
        buf = next_buf;
        match outcome {
            Ok(ReadOutcome::Frames { count, .. }) => return (audio, count.get()),
            Ok(ReadOutcome::Eof { .. }) => return (audio, 0),
            Ok(ReadOutcome::Pending { .. }) => {
                time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("decode error while waiting for frames: {error}"),
        }
    }
    panic!("timed out waiting for ReadOutcome::Frames");
}

async fn wait_for_chunk(
    mut audio: RegisteredAudio<Stream<MemStream>, TestPools>,
    budget: Duration,
) -> (RegisteredAudio<Stream<MemStream>, TestPools>, AudioChunk) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (next_audio, outcome) = blocking_audio(audio, |audio| audio.next_chunk()).await;
        audio = next_audio;
        match outcome.expect("decode while waiting for a PCM chunk") {
            ChunkOutcome::Chunk(chunk) => return (audio, chunk),
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
    audio: RegisteredAudio<Stream<MemStream>, TestPools>,
) -> (RegisteredAudio<Stream<MemStream>, TestPools>, ReadOutcome) {
    let mut buf = [0.0f32; 256];
    let (audio, (_buf, outcome)) = blocking_audio(audio, move |audio| {
        let outcome = audio.read(&mut buf);
        (buf, outcome)
    })
    .await;
    (audio, outcome.expect("read"))
}

async fn drain_to_eof(
    mut audio: RegisteredAudio<Stream<MemStream>, TestPools>,
    budget: Duration,
) -> (RegisteredAudio<Stream<MemStream>, TestPools>, usize) {
    let mut buf = [0.0f32; 4096];
    let mut total = 0usize;
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (next_audio, (next_buf, outcome)) = blocking_audio(audio, move |audio| {
            let outcome = audio.read(&mut buf);
            (buf, outcome)
        })
        .await;
        audio = next_audio;
        buf = next_buf;
        match outcome {
            Ok(ReadOutcome::Frames { count, .. }) => total += count.get(),
            Ok(ReadOutcome::Eof { .. }) => return (audio, total),
            Ok(ReadOutcome::Pending { .. }) => {
                time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!("decode error while draining: {error}"),
        }
    }
    panic!("timed out before reaching Eof; collected {total} frames");
}

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn basic_decode_to_eof() {
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let config = wav_stream(8_000);
    let audio = worker.open(config).await.expect("audio construction");

    let (_audio, frames) = drain_to_eof(audio, Duration::from_secs(5)).await;
    assert!(
        frames >= 8_000,
        "expected at least the input frame count, got {frames}"
    );
}

#[kithara::test(
    tokio,
    flash(false),
    timeout(Duration::from_secs(15)),
    hang_timeout_secs(5)
)]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
async fn non_unity_route_change_resumes_ahead_of_the_consumer(#[case] backend: StretchKind) {
    const PRELOAD_CHUNKS: usize = 64;
    const RING_CHUNKS: usize = 64;
    const SOURCE_RATE: u32 = 44_100;
    const TARGET_RATE: u32 = 48_000;

    let source_rate = NonZeroU32::new(SOURCE_RATE).expect("source rate is non-zero");
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("target rate is non-zero");
    let source_frames = usize::try_from(SOURCE_RATE).expect("source rate fits usize") * 6;
    let wav = signal::wav(SOURCE_RATE, 2, source_frames, signal::TONE);
    let stream = MemStreamConfig {
        source: Some(MemorySource::new(wav)),
        event_bus: None,
    };
    let audio = AudioConfig::<MemStream, RubatoBackend>::for_stream(stream)
        .host_sample_rate(source_rate)
        .media_info(
            MediaInfo::builder()
                .channels(2)
                .codec(AudioCodec::Pcm)
                .container(ContainerFormat::Wav)
                .sample_rate(SOURCE_RATE)
                .build(),
        )
        .preload_chunks(NonZeroUsize::new(PRELOAD_CHUNKS).expect("preload count is non-zero"))
        .audio_buffer_chunks(RING_CHUNKS)
        .consumer_wake_mode(ConsumerWakeMode::ImmediateOffRt)
        .hint("wav".to_owned())
        .build();
    let controls = StretchControls::new(0.5);
    controls.set_backend(backend);
    controls.set_keylock(true);
    let config = TrackConfig::for_audio(audio)
        .warp(WarpConfig::builder().stretch(controls).build())
        .build();
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let audio = worker.open(config).await.expect("audio construction");
    let mut events = audio.event_bus().subscribe();
    let gate = audio
        .preload_gate()
        .expect("worker-backed audio exposes its preload gate");
    time::timeout(
        Duration::from_secs(5),
        gate.wait_for_epoch(audio.preload_epoch()),
    )
    .await
    .expect("non-unity source must preload");

    let (audio, first) = wait_for_chunk(audio, Duration::from_secs(2)).await;
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
        decoded_frontier.saturating_sub(committed) > Duration::from_millis(250),
        "fixture needs admitted PCM well ahead of the consumer: committed={committed:?}, decoded_frontier={decoded_frontier:?}"
    );

    audio.set_host_sample_rate(target_rate);
    let (mut audio, _queued) = wait_for_chunk(audio, Duration::from_secs(2)).await;
    let committed_at_route = audio.position();
    let decoded_at_route = audio.decoded_frontier();
    assert!(
        decoded_at_route.saturating_sub(committed_at_route) > Duration::from_millis(250),
        "route must be selected while admitted PCM remains ahead of the consumer: committed={committed_at_route:?}, decoded_frontier={decoded_at_route:?}",
    );

    loop {
        let envelope = events.recv().await.expect("decoder event bus remains open");
        if matches!(
            envelope.event,
            Event::Decoder(DecoderEvent::DecoderChanged {
                cause: DecoderChangeCause::HostRateChange,
                ..
            })
        ) {
            break;
        }
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let rebuilt = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for rebuilt PCM");
        let (next_audio, chunk) = wait_for_chunk(audio, remaining).await;
        audio = next_audio;
        if chunk.meta.spec.sample_rate == target_rate {
            break chunk;
        }
    };
    assert!(
        rebuilt.meta.timestamp >= committed_at_route.saturating_add(resume_margin),
        "route recreation must resume from admitted Warp progress, not the consumer head"
    );
    assert!(
        rebuilt.meta.timestamp < decoded_frontier,
        "route recreation must resume before the raw decoder frontier while Warp retains backend latency"
    );
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(10)),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
async fn seek_during_active_decode_completes_without_hang() {
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let config = wav_stream(44_100 * 3);
    let audio = worker.open(config).await.expect("audio construction");
    let mut events = audio.event_bus().subscribe();

    let (audio, _initial_frames) = wait_for_frames(audio, Duration::from_secs(2)).await;
    let (mut audio, seek_result) =
        blocking_audio(audio, |audio| audio.seek(Duration::from_secs_f64(1.5))).await;
    seek_result.expect("seek");

    let mut observed_epoch: Option<SeekEpoch> = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Ok(Ok(Event::Audio(AudioEvent::SeekLifecycle {
            stage: SeekLifecycleStage::SeekRequest,
            seek_epoch,
            ..
        }))) = time::timeout(remaining, events.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            observed_epoch = Some(seek_epoch);
            break;
        }
    }
    let expected_epoch = observed_epoch.expect("SeekLifecycle::SeekRequest event");

    // `SeekComplete` is published from the first output pass after the seek, so
    // the consumer has to keep reading: waiting on the bus without reading waits
    // for an event nothing will produce.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_complete = false;
    while Instant::now() < deadline && !saw_complete {
        let (next_audio, outcome) = pump_once(audio).await;
        audio = next_audio;
        if matches!(outcome, ReadOutcome::Pending { .. }) {
            time::sleep(Duration::from_millis(20)).await;
        }
        while let Ok(envelope) = events.try_recv() {
            if let Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }) = envelope.event
                && seek_epoch == expected_epoch
            {
                saw_complete = true;
                break;
            }
        }
    }
    assert!(saw_complete, "SeekComplete must arrive after seek");

    let (_audio, frames_after) = wait_for_frames(audio, Duration::from_secs(2)).await;
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
async fn rapid_seeks_via_timeline_all_complete() {
    const SEEK_COUNT: usize = 6;

    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let config = wav_stream(44_100 * 4);
    let mut audio = worker.open(config).await.expect("audio construction");
    let mut events = audio.event_bus().subscribe();

    // Keep the settle reads inline so the flash rewriter retargets these
    // sleeps onto the virtual clock. `Audio::seek()` publishes SeekRequest
    // synchronously; SeekComplete / PlaybackProgress still require reads to
    // commit post-seek output.

    // Prime: read until the first decoded frames arrive.
    {
        let mut buf = [0.0f32; 256];
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let (next_audio, (next_buf, outcome)) = blocking_audio(audio, move |audio| {
                let outcome = audio.read(&mut buf);
                (buf, outcome)
            })
            .await;
            audio = next_audio;
            buf = next_buf;
            match outcome {
                Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Eof { .. }) => break,
                Ok(ReadOutcome::Pending { .. }) => {
                    time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("decode error while priming: {error}"),
            }
        }
    }

    let mut expected_epochs = Vec::with_capacity(SEEK_COUNT);
    for i in 0..SEEK_COUNT {
        let target = Duration::from_millis(200 + (i as u64) * 250);
        let (next_audio, seek_result) =
            blocking_audio(audio, move |audio| audio.seek(target)).await;
        audio = next_audio;
        seek_result.expect("seek");

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut captured = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(Ok(Event::Audio(AudioEvent::SeekLifecycle {
                stage: SeekLifecycleStage::SeekRequest,
                seek_epoch,
                ..
            }))) = time::timeout(remaining, events.recv())
                .await
                .map(|r| r.map(|env| env.event))
            {
                captured = Some(seek_epoch);
                break;
            }
        }
        expected_epochs.push(captured.expect("seek epoch from SeekRequest"));

        // Settle on the virtual clock: read post-seek frames so the consumer
        // commits this seek and the worker advances before the next `seek()`.
        let mut buf = [0.0f32; 256];
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            let (next_audio, (next_buf, outcome)) = blocking_audio(audio, move |audio| {
                let outcome = audio.read(&mut buf);
                (buf, outcome)
            })
            .await;
            audio = next_audio;
            buf = next_buf;
            match outcome {
                Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Eof { .. }) => break,
                Ok(ReadOutcome::Pending { .. }) => {
                    time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("decode error while settling seek: {error}"),
            }
        }
    }

    let highest_expected = *expected_epochs
        .iter()
        .max()
        .expect("at least one seek epoch");

    let mut buf = [0.0f32; 256];
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last_complete: Option<SeekEpoch> = None;
    while Instant::now() < deadline {
        // Read each tick so the consumer keeps committing post-seek output and
        // emitting `SeekComplete` for the highest requested epoch. The
        // ownership roundtrip keeps a possible read park off the runtime;
        // `events.recv()` then yields on the virtual clock for the next chunk.
        let (next_audio, (next_buf, outcome)) = blocking_audio(audio, move |audio| {
            let outcome = audio.read(&mut buf);
            (buf, outcome)
        })
        .await;
        audio = next_audio;
        buf = next_buf;
        match outcome {
            Ok(ReadOutcome::Frames { .. })
            | Ok(ReadOutcome::Eof { .. })
            | Ok(ReadOutcome::Pending { .. }) => {}
            Err(error) => panic!("decode error while draining seek completions: {error}"),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match time::timeout(remaining, events.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            Ok(Ok(Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }))) => {
                last_complete = Some(seek_epoch);
                if seek_epoch >= highest_expected {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // Paced blocking reads can exhaust the virtual-clock budget before the
    // loop drains its own subscriber queue; events already delivered before
    // the deadline still count toward the contract.
    while let Ok(envelope) = events.try_recv() {
        if let Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }) = envelope.event {
            last_complete = Some(seek_epoch);
        }
    }
    assert_eq!(
        last_complete,
        Some(highest_expected),
        "last observed SeekComplete must match the highest requested epoch"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn truncated_wav_surfaces_decode_error_or_eof() {
    let region = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let mut wav = signal::wav(44_100, 2, 44_100, signal::TONE);
    wav.truncate(wav.len() / 4);
    let source = MemorySource::new(wav);
    let config = AudioConfig::<MemStream>::for_stream(MemStreamConfig {
        source: Some(source),
        event_bus: None,
    })
    .hint("wav".to_string())
    .build();

    let audio = worker.open(config).await.expect("audio construction");

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
