#![cfg(not(target_arch = "wasm32"))]

use std::{fs::File, io::Write};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    bufpool::{PcmPool, SharedPool},
    decode::{GaplessMode, SilenceTrimParams},
    events::{AudioEvent, Event, EventReceiver, SeekEpoch, SeekLifecycleStage},
    file::{FileConfig, FileSrc},
    platform::time::{self, Duration, Instant},
    stream::{ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{TestTempDir, create_test_wav, kithara};
use tempfile::NamedTempFile;

/// Polls `audio.read()` until it returns `Frames`, an unrelated `Eof`,
/// or the deadline expires. Returns the number of frames read on
/// success.
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

/// Drains events until a `SeekLifecycle::SeekRequest` arrives, returning its epoch.
async fn await_seek_request_epoch(events: &mut EventReceiver, budget: Duration) -> SeekEpoch {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Ok(Ok(Event::Audio(AudioEvent::SeekLifecycle {
            stage: SeekLifecycleStage::SeekRequest,
            seek_epoch,
            ..
        }))) = time::timeout(remaining, events.recv()).await
        {
            return seek_epoch;
        }
    }
    panic!("SeekLifecycle::SeekRequest was not observed");
}

/// Write test WAV to a temp file and return config for it.
///
/// The returned [`TestTempDir`] owns an isolated cache directory —
/// callers keep it alive for the lifetime of the test so the shared
/// app cache at `env::temp_dir()/kithara` stays untouched, and the
/// directory is auto-deleted when the test returns.
fn test_wav_config(
    sample_count: usize,
) -> (TestTempDir, NamedTempFile, AudioConfig<kithara::file::File>) {
    let wav_data = create_test_wav(sample_count, 44100, 2);
    let tmp = NamedTempFile::new().unwrap();
    File::create(tmp.path())
        .unwrap()
        .write_all(&wav_data)
        .unwrap();
    let cache = TestTempDir::new();
    let file_config = FileConfig::for_src(FileSrc::Local(tmp.path().to_path_buf()))
        .store(StoreOptions::new(cache.path()))
        .build();
    let config = AudioConfig::<kithara::file::File>::for_stream(file_config)
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(PcmPool::default())
        .hint("wav".to_string())
        .build();
    (cache, tmp, config)
}

#[kithara::test(tokio)]
#[case::short(16)]
#[case::regular(1000)]
async fn test_audio_new(#[case] sample_count: usize) {
    let (_cache, _tmp, config) = test_wav_config(sample_count);
    let _audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();
}

/// `Audio::new` pre-warms its PCM pool so the decode hot path and the
/// first reads reuse pooled buffers instead of allocating on the audio
/// thread. Drives a fresh, cold custom pool and asserts construction
/// leaves it warmed.
#[kithara::test(tokio)]
async fn audio_new_warms_pcm_pool() {
    let pool: PcmPool = SharedPool::<8, Vec<f32>>::new(128, 200_000);
    assert_eq!(
        pool.allocated_bytes(),
        0,
        "precondition: a fresh pool is cold"
    );

    let (_cache, _tmp, mut config) = test_wav_config(1000);
    config.pcm_pool = pool.clone();

    let _audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    assert!(
        pool.allocated_bytes() > 0,
        "Audio::new must pre-warm its PCM pool (allocated_bytes still 0)"
    );

    // The warm-up's payoff: decode-sized buffers come back as pool hits,
    // not fresh allocations on the audio thread.
    let misses_before = pool.stats().alloc_misses;
    let decode_samples = 4608 * 2;
    let bufs: Vec<_> = (0..8)
        .map(|_| pool.get_with(|b| b.resize(decode_samples, 0.0)))
        .collect();
    let misses_after = pool.stats().alloc_misses;
    assert_eq!(
        misses_before, misses_after,
        "a warmed pool must serve decode-sized buffers without allocating"
    );
    drop(bufs);
}

#[kithara::test]
fn test_audio_config_with_media_info() {
    let info = MediaInfo::builder()
        .container(ContainerFormat::Wav)
        .sample_rate(44100)
        .build();

    let config = AudioConfig::<kithara::file::File>::for_stream(FileConfig::default())
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(PcmPool::default())
        .media_info(info.clone())
        .build();

    assert!(config.media_info.is_some());
    assert_eq!(
        config.media_info.unwrap().container,
        Some(ContainerFormat::Wav)
    );
}

#[kithara::test]
#[case::codec_priming(GaplessMode::CodecPriming)]
#[case::silence_trim(GaplessMode::SilenceTrim(SilenceTrimParams {
    threshold_db: 50.0,
    min_trim_frames: 128,
    scan_window_frames: 2_048,
    trim_trailing: true,
}))]
fn test_audio_config_with_gapless_mode(#[case] mode: GaplessMode) {
    let config = AudioConfig::<kithara::file::File>::for_stream(FileConfig::default())
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(PcmPool::default())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .gapless_mode(mode)
                .build(),
        )
        .build();

    assert_eq!(config.decoder.gapless_mode, mode);
}

#[kithara::test(tokio)]
async fn test_audio_spec() {
    let (_cache, _tmp, config) = test_wav_config(1000);
    let audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let spec = audio.spec();
    assert_eq!(spec.sample_rate.get(), 44100);
    assert_eq!(spec.channels, 2);
}

#[kithara::test(tokio, timeout(Duration::from_secs(20)))]
async fn test_audio_read() {
    let (_cache, _tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = [0.0f32; 256];
    let mut total_read = 0usize;

    let saw_eof = loop {
        match audio.read(&mut buf).expect("read") {
            ReadOutcome::Pending { .. } => break false,
            ReadOutcome::Frames { count, .. } => total_read += count.get(),
            ReadOutcome::Eof { .. } => break true,
        }
    };

    assert!(total_read > 0);
    assert!(saw_eof);
}

#[kithara::test(tokio)]
#[case::tiny(100, 4)]
#[case::wide(1000, 64)]
async fn test_audio_read_small_buffer(#[case] sample_count: usize, #[case] buf_len: usize) {
    let (_cache, _tmp, config) = test_wav_config(sample_count);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = vec![0.0f32; buf_len];
    let outcome = audio.read(&mut buf).expect("read");
    let ReadOutcome::Frames { count, .. } = outcome else {
        panic!("expected Frames, got {outcome:?}");
    };

    assert_eq!(count.get(), buf_len);
}

#[kithara::test(tokio)]
async fn test_audio_is_eof() {
    let (_cache, _tmp, config) = test_wav_config(10);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = [0.0f32; 1024];
    let saw_eof = loop {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => break false,
            Ok(ReadOutcome::Frames { .. }) => continue,
            Ok(ReadOutcome::Eof { .. }) => break true,
            Err(e) => panic!("decode error: {e}"),
        }
    };
    assert!(saw_eof, "expected ReadOutcome::Eof after draining WAV");
}

#[kithara::test(tokio)]
async fn test_audio_seek() {
    let (_cache, _tmp, config) = test_wav_config(44100);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = [0.0f32; 256];
    let _ = audio.read(&mut buf);

    let result = audio.seek(Duration::from_secs(0));
    assert!(result.is_ok());

    assert!(matches!(
        audio.read(&mut buf),
        Ok(ReadOutcome::Frames { .. })
    ));
}

#[kithara::test(tokio)]
async fn test_audio_playback_progress_uses_output_commit() {
    let (_cache, _tmp, config) = test_wav_config(1024);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut events = audio.event_bus().subscribe();
    let mut buf = [0.0f32; 256];

    let _ = audio.read(&mut buf);

    let mut saw_progress = false;
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        if let Ok(Ok(Event::Audio(AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            seek_epoch,
            ..
        }))) = time::timeout(Duration::from_millis(40), events.recv()).await
        {
            assert!(position_ms > 0);
            assert!(total_ms.is_some());
            assert_eq!(seek_epoch, 0);
            saw_progress = true;
            break;
        }
    }

    assert!(saw_progress, "playback progress event was not observed");
}

#[kithara::test(tokio)]
async fn test_seek_emits_matching_playback_progress() {
    let (_cache, _tmp, config) = test_wav_config(44_100 * 4);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut events = audio.event_bus().subscribe();
    let mut buf = [0.0f32; 256];

    audio.seek(Duration::from_secs_f64(2.5)).unwrap();
    let expected_epoch = await_seek_request_epoch(&mut events, Duration::from_secs(1)).await;
    let _ = audio.read(&mut buf);

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut matched_epoch = None;
    while Instant::now() < deadline {
        if let Ok(Ok(Event::Audio(AudioEvent::PlaybackProgress { seek_epoch, .. }))) =
            time::timeout(Duration::from_millis(40), events.recv()).await
            && seek_epoch == expected_epoch
        {
            matched_epoch = Some(seek_epoch);
            break;
        }
    }

    assert_eq!(matched_epoch, Some(expected_epoch));
}

#[kithara::test(tokio)]
async fn test_seek_complete_emitted_only_after_output_commit() {
    let (_cache, _tmp, config) = test_wav_config(44_100 * 4);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    let mut events = audio.event_bus().subscribe();
    audio.seek(Duration::from_secs_f64(1.5)).unwrap();
    let expected_epoch = await_seek_request_epoch(&mut events, Duration::from_secs(1)).await;

    let mut saw_seek_complete_before_read = false;
    while let Ok(event) = events.try_recv() {
        if matches!(event, Event::Audio(AudioEvent::SeekComplete { .. })) {
            saw_seek_complete_before_read = true;
            break;
        }
    }
    assert!(
        !saw_seek_complete_before_read,
        "SeekComplete must not be emitted before output commit"
    );

    let mut buf = [0.0f32; 512];
    let read_result = audio.read(&mut buf);
    assert!(
        matches!(read_result, Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0),
        "read must commit PCM output",
    );

    let deadline = Instant::now() + Duration::from_millis(400);
    let mut saw_seek_complete = false;
    let mut saw_output_committed = false;
    while Instant::now() < deadline {
        match time::timeout(Duration::from_millis(40), events.recv()).await {
            Ok(Ok(Event::Audio(AudioEvent::SeekLifecycle {
                stage: SeekLifecycleStage::OutputCommitted,
                seek_epoch,
                ..
            }))) => {
                assert_eq!(seek_epoch, expected_epoch);
                saw_output_committed = true;
            }
            Ok(Ok(Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }))) => {
                assert_eq!(seek_epoch, expected_epoch);
                saw_seek_complete = true;
                break;
            }
            _ => {}
        }
    }

    assert!(
        saw_output_committed,
        "expected OutputCommitted lifecycle event"
    );
    assert!(
        saw_seek_complete,
        "expected SeekComplete after output commit"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
#[case::single(false)]
#[case::idempotent(true)]
async fn test_audio_preload(#[case] second_preload: bool) {
    let (_cache, _tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    assert!(
        !audio.is_preloaded(),
        "fresh Audio must not advertise preloaded state"
    );

    audio.preload().expect("preload must succeed");
    if second_preload {
        audio.preload().expect("second preload must succeed");
    }

    assert!(audio.is_preloaded(), "preload must flip the latch");

    let frames = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    assert!(frames > 0, "preload must produce at least one frame");
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn test_audio_preload_rearms_after_seek() {
    let (_cache, _tmp, config) = test_wav_config(44_100);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .unwrap();

    audio.preload().expect("preload must succeed");
    let initial = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    assert!(initial > 0, "initial read must produce samples");

    audio.seek(Duration::from_millis(100)).unwrap();
    audio.preload().expect("preload after seek must succeed");

    let after_seek = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    assert!(
        after_seek > 0,
        "seek must re-arm preload so the next read produces samples"
    );
}

/// `preloaded` is a one-way latch: once set by `preload()`, it must
/// survive seeks.  If seek resets it, the audio callback switches to
/// blocking recv — parking the thread on every empty ringbuf poll.
#[kithara::test(tokio)]
async fn preloaded_survives_seek() {
    let (_cache, _tmp, config) = test_wav_config(44100 * 2);
    let mut audio = Audio::<Stream<kithara::file::File>>::new(config)
        .await
        .expect("create audio");

    audio.preload().expect("preload must succeed");
    let initial = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    assert!(initial > 0, "preload must deliver before seek");
    assert!(audio.is_preloaded());

    audio.seek(Duration::from_millis(100)).unwrap();
    assert!(audio.is_preloaded(), "seek must not reset preloaded");

    let after_seek = wait_for_frames(&mut audio, Duration::from_secs(2)).await;
    assert!(after_seek > 0, "must read samples after seek");
}
