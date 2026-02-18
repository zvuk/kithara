use kithara_stream::Stream;
use kithara_test_utils::create_test_wav;
use rstest::rstest;

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

#[rstest]
#[case::short(16)]
#[case::regular(1000)]
#[tokio::test]
async fn test_audio_new(#[case] sample_count: usize) {
    let (_tmp, config) = test_wav_config(sample_count);
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

#[rstest]
#[case::tiny(100, 4)]
#[case::wide(1000, 64)]
#[tokio::test]
async fn test_audio_read_small_buffer(#[case] sample_count: usize, #[case] buf_len: usize) {
    let (_tmp, config) = test_wav_config(sample_count);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    let mut buf = vec![0.0f32; buf_len];
    let n = audio.read(&mut buf);

    assert_eq!(n, buf_len);
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

#[rstest]
#[case::single(false)]
#[case::idempotent(true)]
#[tokio::test]
async fn test_audio_preload(#[case] second_preload: bool) {
    let (_tmp, config) = test_wav_config(1000);
    let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
        .await
        .unwrap();

    assert!(audio.current_chunk.is_none());

    let notify = audio.preload_notify.clone();
    notify.notified().await;
    audio.preload();
    if second_preload {
        audio.preload();
    }

    assert!(audio.current_chunk.is_some());

    let mut buf = [0.0f32; 64];
    let n = audio.read(&mut buf);
    assert!(n > 0);
}
