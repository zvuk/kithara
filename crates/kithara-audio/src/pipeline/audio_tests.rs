use kithara_stream::Stream;
use kithara_test_utils::create_test_wav;

use super::*;

/// Write test WAV to a temp file and return config for it.
fn test_wav_config(
    sample_count: usize,
) -> (tempfile::NamedTempFile, AudioConfig<kithara_file::File>) {
    let wav_data = create_test_wav(sample_count, 44100, 2);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut std::fs::File::create(tmp.path()).unwrap(), &wav_data).unwrap();
    let file_config =
        kithara_file::FileConfig::new(kithara_file::FileSrc::Local(tmp.path().to_path_buf()));
    let config = AudioConfig::<kithara_file::File>::new(file_config).with_hint("wav");
    (tmp, config)
}

#[tokio::test]
async fn test_audio_new() {
    let (_tmp, config) = test_wav_config(1000);
    let _audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_audio_receive_chunks() {
    let (_tmp, config) = test_wav_config(1000);
    let audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut chunk_count = 0;
    while let Ok(fetch) = audio.pcm_rx().recv() {
        if fetch.is_eof() {
            break;
        }
        chunk_count += 1;
        assert!(!fetch.data.pcm.is_empty());
        if chunk_count >= 5 {
            break;
        }
    }

    assert!(chunk_count > 0);
}

#[test]
fn test_audio_config_with_media_info() {
    let info = kithara_stream::MediaInfo::default()
        .with_container(kithara_stream::ContainerFormat::Wav)
        .with_sample_rate(44100);

    let config = AudioConfig::<kithara_file::File>::new(kithara_file::FileConfig::default())
        .with_media_info(info.clone());

    assert!(config.media_info.is_some());
    assert_eq!(
        config.media_info.unwrap().container,
        Some(kithara_stream::ContainerFormat::Wav)
    );
}

#[tokio::test]
async fn test_audio_spec() {
    let (_tmp, config) = test_wav_config(1000);
    let audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let spec = audio.spec();
    assert_eq!(spec.sample_rate, 44100);
    assert_eq!(spec.channels, 2);
}

#[tokio::test]
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

#[tokio::test]
async fn test_audio_read_small_buffer() {
    let (_tmp, config) = test_wav_config(100);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    // Read with very small buffer
    let mut buf = [0.0f32; 4];
    let n = audio.read(&mut buf);

    assert_eq!(n, 4);
}

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
async fn test_audio_preload() {
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    // Before preload, current_chunk should be None
    assert!(audio.current_chunk.is_none());

    // Get notify and await it
    let notify = audio.preload_notify.clone();
    notify.notified().await;
    audio.preload();

    // After preload, current_chunk should have data
    assert!(audio.current_chunk.is_some());

    // First read should return data immediately
    let mut buf = [0.0f32; 64];
    let n = audio.read(&mut buf);
    assert!(n > 0);
}

#[tokio::test]
async fn test_audio_preload_idempotent() {
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    // Preload first time
    let notify = audio.preload_notify.clone();
    notify.notified().await;
    audio.preload();
    assert!(audio.current_chunk.is_some());

    // Preload again — should be no-op since chunk is already loaded
    audio.preload();
    assert!(audio.current_chunk.is_some());

    // Reading should still work normally
    let mut buf = [0.0f32; 64];
    let n = audio.read(&mut buf);
    assert!(n > 0);
}
