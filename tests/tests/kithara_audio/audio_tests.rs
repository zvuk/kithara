#![cfg(not(target_arch = "wasm32"))]

use std::{fs::File, io::Write};

use kithara_audio::internal::audio::*;
use kithara_events::{AudioEvent, SeekLifecycleStage};
use kithara_platform::time::{Duration, Instant, sleep, timeout};
use kithara_stream::{ContainerFormat, MediaInfo, Stream};
use kithara_test_utils::{create_test_wav, kithara};
use ringbuf::traits::Consumer;

/// Write test WAV to a temp file and return config for it.
fn test_wav_config(
    sample_count: usize,
) -> (tempfile::NamedTempFile, AudioConfig<kithara_file::File>) {
    let wav_data = create_test_wav(sample_count, 44100, 2);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    File::create(tmp.path())
        .unwrap()
        .write_all(&wav_data)
        .unwrap();
    let file_config =
        kithara_file::FileConfig::new(kithara_file::FileSrc::Local(tmp.path().to_path_buf()));
    let config = AudioConfig::<kithara_file::File>::new(file_config).with_hint("wav");
    (tmp, config)
}

#[kithara::test(tokio)]
#[case::short(16)]
#[case::regular(1000)]
async fn test_audio_new(#[case] sample_count: usize) {
    let (_tmp, config) = test_wav_config(sample_count);
    let _audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();
}

#[kithara::test(tokio)]
async fn test_audio_receive_chunks() {
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut chunk_count = 0;
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Some(fetch) = audio.pcm_rx().try_pop() {
            if fetch.is_eof() {
                break;
            }
            chunk_count += 1;
            assert!(!fetch.data.pcm.is_empty());
            if chunk_count >= 5 {
                break;
            }
        } else {
            sleep(Duration::from_millis(1)).await;
        }
    }

    assert!(chunk_count > 0);
}

#[kithara::test]
fn test_audio_config_with_media_info() {
    let info = MediaInfo::default()
        .with_container(ContainerFormat::Wav)
        .with_sample_rate(44100);

    let config = AudioConfig::<kithara_file::File>::new(kithara_file::FileConfig::default())
        .with_media_info(info.clone());

    assert!(config.media_info.is_some());
    assert_eq!(
        config.media_info.unwrap().container,
        Some(ContainerFormat::Wav)
    );
}

#[kithara::test(tokio)]
async fn test_audio_spec() {
    let (_tmp, config) = test_wav_config(1000);
    let audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let spec = audio.spec();
    assert_eq!(spec.sample_rate, 44100);
    assert_eq!(spec.channels, 2);
}

#[kithara::test(tokio)]
async fn test_audio_read() {
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = [0.0f32; 256];
    let mut total_read = 0;

    while !audio.is_eof() {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        total_read += n;
    }

    assert!(total_read > 0);
    assert!(audio.is_eof());
}

#[kithara::test(tokio)]
#[case::tiny(100, 4)]
#[case::wide(1000, 64)]
async fn test_audio_read_small_buffer(#[case] sample_count: usize, #[case] buf_len: usize) {
    let (_tmp, config) = test_wav_config(sample_count);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = vec![0.0f32; buf_len];
    let n = audio.read(&mut buf);

    assert_eq!(n, buf_len);
}

#[kithara::test(tokio)]
async fn test_audio_is_eof() {
    let (_tmp, config) = test_wav_config(10);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    assert!(!audio.is_eof());

    // Read all data
    let mut buf = [0.0f32; 1024];
    while audio.read(&mut buf) > 0 {}

    assert!(audio.is_eof());
}

#[kithara::test(tokio)]
async fn test_audio_seek() {
    let (_tmp, config) = test_wav_config(44100); // 1 second
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    // Read some data
    let mut buf = [0.0f32; 256];
    audio.read(&mut buf);

    // Seek to beginning
    let result = audio.seek(Duration::from_secs(0));
    assert!(result.is_ok());

    // Should be able to read again
    assert!(!audio.is_eof());
}

#[kithara::test(tokio)]
async fn test_rodio_source_try_seek() {
    let (_tmp, config) = test_wav_config(44100);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let result = ::rodio::Source::try_seek(&mut audio, Duration::from_millis(250));
    assert!(
        result.is_ok(),
        "rodio::Source::try_seek must delegate to Audio::seek"
    );

    let mut buf = [0.0f32; 256];
    assert!(audio.read(&mut buf) > 0);
}

#[kithara::test(tokio)]
async fn test_audio_playback_progress_uses_output_commit() {
    let (_tmp, config) = test_wav_config(1024);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut events = audio.decode_events();
    let mut buf = [0.0f32; 256];

    let _ = audio.read(&mut buf);

    let mut saw_progress = false;
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(40), events.recv()).await {
            Ok(Ok(event)) => {
                if let AudioEvent::PlaybackProgress {
                    position_ms,
                    total_ms,
                    seek_epoch,
                } = event
                {
                    assert!(position_ms > 0);
                    assert!(total_ms.is_some());
                    assert_eq!(seek_epoch, 0);
                    saw_progress = true;
                    break;
                }
            }
            _ => {}
        }
    }

    assert!(saw_progress, "playback progress event was not observed");
}

#[kithara::test(tokio)]
async fn test_seek_emits_matching_playback_progress() {
    let (_tmp, config) = test_wav_config(44_100 * 4);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut events = audio.decode_events();
    let mut buf = [0.0f32; 256];

    // Epoch comes from Timeline now, not from caller.
    audio.seek(Duration::from_secs_f64(2.5)).unwrap();
    let expected_epoch = seek_epoch(&audio);
    let _ = audio.read(&mut buf);

    let deadline = Instant::now() + Duration::from_millis(300);
    let mut matched_epoch = None;
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(40), events.recv()).await {
            Ok(Ok(AudioEvent::PlaybackProgress { seek_epoch, .. })) => {
                matched_epoch = Some(seek_epoch);
                break;
            }
            _ => {}
        }
    }

    assert_eq!(matched_epoch, Some(expected_epoch));
}

#[kithara::test(tokio)]
async fn test_seek_complete_emitted_only_after_output_commit() {
    let (_tmp, config) = test_wav_config(44_100 * 4);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut events = audio.decode_events();
    // Epoch comes from Timeline now, not from caller.
    audio.seek(Duration::from_secs_f64(1.5)).unwrap();
    let expected_epoch = seek_epoch(&audio);

    let mut saw_seek_complete_before_read = false;
    while let Ok(event) = events.try_recv() {
        if matches!(event, AudioEvent::SeekComplete { .. }) {
            saw_seek_complete_before_read = true;
            break;
        }
    }
    assert!(
        !saw_seek_complete_before_read,
        "SeekComplete must not be emitted before output commit"
    );

    let mut buf = [0.0f32; 512];
    let n = audio.read(&mut buf);
    assert!(n > 0, "read must commit PCM output");

    let deadline = Instant::now() + Duration::from_millis(400);
    let mut saw_seek_complete = false;
    let mut saw_output_committed = false;
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(40), events.recv()).await {
            Ok(Ok(AudioEvent::SeekLifecycle {
                stage: SeekLifecycleStage::OutputCommitted,
                seek_epoch,
                ..
            })) => {
                assert_eq!(seek_epoch, expected_epoch);
                saw_output_committed = true;
            }
            Ok(Ok(AudioEvent::SeekComplete { seek_epoch, .. })) => {
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
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    assert!(!has_current_chunk(&audio));

    let notify = preload_notify(&audio);
    notify.notified().await;
    audio.preload();
    if second_preload {
        audio.preload();
    }

    assert!(has_current_chunk(&audio));

    let mut buf = [0.0f32; 64];
    let n = audio.read(&mut buf);
    assert!(n > 0);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn test_audio_preload_rearms_after_seek() {
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let first_notify = preload_notify(&audio);
    timeout(Duration::from_secs(1), first_notify.notified())
        .await
        .expect("initial preload notify must fire");
    audio.preload();

    let mut buf = [0.0f32; 64];
    let n = audio.read(&mut buf);
    assert!(n > 0, "initial read must produce samples");

    audio.seek(Duration::from_millis(100)).unwrap();

    let second_notify = preload_notify(&audio);
    timeout(Duration::from_secs(1), second_notify.notified())
        .await
        .expect("seek must re-arm preload notify");
}
