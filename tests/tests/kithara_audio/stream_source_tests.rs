#![cfg(not(target_arch = "wasm32"))]

use kithara_audio::{Audio, AudioConfig, ReadOutcome};
use kithara_events::{AudioEvent, Event, SeekEpoch, SeekLifecycleStage};
use kithara_integration_tests::{
    create_test_wav, kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
};
use kithara_platform::time::{self, Duration, Instant};
use kithara_stream::Stream;

fn wav_stream(samples: usize) -> AudioConfig<MemStream> {
    let wav = create_test_wav(samples, 44_100, 2);
    let source = MemorySource::new(wav);
    let stream = MemStreamConfig {
        source: Some(source),
        event_bus: None,
    };
    AudioConfig::<MemStream>::for_stream(stream)
        .hint("wav".to_string())
        .build()
}

async fn wait_for_frames<S>(audio: &mut Audio<S>, budget: Duration) -> usize {
    let mut buf = [0.0f32; 256];
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => return count.get(),
            Ok(ReadOutcome::Eof { .. }) => return 0,
            Ok(ReadOutcome::Pending { .. }) => {
                time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("decode error while waiting for frames: {error}"),
        }
    }
    panic!("timed out waiting for ReadOutcome::Frames");
}

async fn drain_to_eof<S>(audio: &mut Audio<S>, budget: Duration) -> usize {
    let mut buf = [0.0f32; 4096];
    let mut total = 0usize;
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => total += count.get(),
            Ok(ReadOutcome::Eof { .. }) => return total,
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
    let config = wav_stream(8_000);
    let mut audio = Audio::<Stream<MemStream>>::new(config)
        .await
        .expect("audio construction");

    let frames = drain_to_eof(&mut audio, Duration::from_secs(5)).await;
    assert!(
        frames >= 8_000,
        "expected at least the input frame count, got {frames}"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn seek_during_active_decode_completes_without_hang() {
    let config = wav_stream(44_100 * 3);
    let mut audio = Audio::<Stream<MemStream>>::new(config)
        .await
        .expect("audio construction");
    let mut events = audio.event_bus().subscribe();

    let _ = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    audio.seek(Duration::from_secs_f64(1.5)).expect("seek");

    let mut observed_epoch: Option<SeekEpoch> = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Ok(Ok(Event::Audio(AudioEvent::SeekLifecycle {
            stage: SeekLifecycleStage::SeekRequest,
            seek_epoch,
            ..
        }))) = time::timeout(remaining, events.recv()).await
        {
            observed_epoch = Some(seek_epoch);
            break;
        }
    }
    let expected_epoch = observed_epoch.expect("SeekLifecycle::SeekRequest event");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_complete = false;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match time::timeout(remaining, events.recv()).await {
            Ok(Ok(Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. })))
                if seek_epoch == expected_epoch =>
            {
                saw_complete = true;
                break;
            }
            Ok(_) => {
                let _ = wait_for_frames(&mut audio, Duration::from_millis(150)).await;
            }
            Err(_) => break,
        }
    }
    assert!(saw_complete, "SeekComplete must arrive after seek");

    let frames_after = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    assert!(
        frames_after > 0,
        "audio must keep producing frames after seek"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(15)))]
async fn rapid_seeks_via_timeline_all_complete() {
    const SEEK_COUNT: usize = 6;

    let config = wav_stream(44_100 * 4);
    let mut audio = Audio::<Stream<MemStream>>::new(config)
        .await
        .expect("audio construction");
    let mut events = audio.event_bus().subscribe();

    let _ = wait_for_frames(&mut audio, Duration::from_secs(2)).await;

    let mut expected_epochs = Vec::with_capacity(SEEK_COUNT);
    for i in 0..SEEK_COUNT {
        let target = Duration::from_millis(200 + (i as u64) * 250);
        audio.seek(target).expect("seek");

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut captured = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(Ok(Event::Audio(AudioEvent::SeekLifecycle {
                stage: SeekLifecycleStage::SeekRequest,
                seek_epoch,
                ..
            }))) = time::timeout(remaining, events.recv()).await
            {
                captured = Some(seek_epoch);
                break;
            }
        }
        expected_epochs.push(captured.expect("seek epoch from SeekRequest"));

        let _ = wait_for_frames(&mut audio, Duration::from_millis(500)).await;
    }

    let highest_expected = *expected_epochs
        .iter()
        .max()
        .expect("at least one seek epoch");

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last_complete: Option<SeekEpoch> = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match time::timeout(remaining, events.recv()).await {
            Ok(Ok(Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }))) => {
                last_complete = Some(seek_epoch);
                if seek_epoch >= highest_expected {
                    break;
                }
            }
            Ok(_) => {
                let _ = wait_for_frames(&mut audio, Duration::from_millis(80)).await;
            }
            Err(_) => break,
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
    let mut wav = create_test_wav(44_100, 44_100, 2);
    wav.truncate(wav.len() / 4);
    let source = MemorySource::new(wav);
    let config = AudioConfig::<MemStream>::for_stream(MemStreamConfig {
        source: Some(source),
        event_bus: None,
    })
    .hint("wav".to_string())
    .build();

    let mut audio = Audio::<Stream<MemStream>>::new(config)
        .await
        .expect("audio construction");

    let mut buf = [0.0f32; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_terminal = false;
    while Instant::now() < deadline {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Eof { .. }) | Err(_) => {
                saw_terminal = true;
                break;
            }
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {
                // Qualified `time::sleep` so the `#[kithara::test(flash(true))]`
                // body rewriter virtualizes this wait to match the already-
                // virtualized `Instant::now()` deadline above. A bare-imported
                // `sleep` is a single-segment path the rewriter does not match,
                // leaving it REAL — a mixed driver clock whose virtual deadline
                // races past while the driver sleeps real, exiting the loop
                // before the worker's EOF marker is drained.
                time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    assert!(
        saw_terminal,
        "truncated WAV must surface either Eof or DecodeError"
    );
}
