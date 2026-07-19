#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    assets::{AssetStoreBuilder, StorageBackend},
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    file::{File, FileConfig},
    platform::{sync::Arc, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{Content, Delivery, FixtureBehavior, TestServerHelper};

use crate::common::test_defaults::Consts;

#[kithara::test(tokio)]
#[case::sw_ext_hint(Some("audio.mp3"), Some("mp3"), DecoderBackend::Symphonia)]
#[case::sw_ext(Some("audio.mp3"), None, DecoderBackend::Symphonia)]
#[case::sw_no_ext_hint(None, Some("mp3"), DecoderBackend::Symphonia)]
#[case::sw_no_ext(None, None, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw_ext_hint(Some("audio.mp3"), Some("mp3"), DecoderBackend::Apple)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw_no_ext(None, None, DecoderBackend::Apple)
)]
async fn audio_file_mp3_decodes_with_duration(
    #[case] suffix: Option<&str>,
    #[case] hint: Option<&str>,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(Consts::TEST_MP3_BYTES.to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let url = match suffix {
        Some(s) => handle.child_url(s),
        None => handle.url(),
    };
    let file_config = FileConfig::for_src(url.clone().into())
        .store(
            AssetStoreBuilder::default()
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let config = AudioConfig::<File>::for_stream(file_config)
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_hint(hint.map(str::to_owned))
        .build();
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .unwrap_or_else(|e| panic!("probe failed for url={url} hint={hint:?}: {e}"));

    let duration = audio.duration();
    assert!(
        duration.is_some(),
        "url={url} hint={hint:?}: duration must be reported (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        (dur_secs - Consts::TEST_MP3_DURATION_SECS).abs() < 2.0,
        "url={url} hint={hint:?}: expected ~{}s, got {dur_secs:.1}s",
        Consts::TEST_MP3_DURATION_SECS
    );

    let (samples_read, position, eof) = spawn_blocking(move || {
        let mut total = 0usize;
        let mut buf = [0.0f32; 4096];
        let mut saw_eof = false;

        for _ in 0..600 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => break,
                Ok(ReadOutcome::Frames { count, .. }) => {
                    total += count.get();
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Err(e) => panic!("decode error: {e}"),
            }
            if audio.position() >= Duration::from_secs(2) {
                break;
            }
        }

        (total, audio.position(), saw_eof)
    })
    .await
    .unwrap();

    assert!(samples_read > 0, "no decoded samples");
    assert!(
        position >= Duration::from_secs(2),
        "url={url} hint={hint:?}: playback ended too early: \
         pos={position:?} eof={eof} samples={samples_read}"
    );
}

/// Duration must be correct IMMEDIATELY after `Audio::new` — before any
/// decode calls. This is what the GUI reads to show track length.
///
/// Uses throttled server: Content-Length is sent immediately but body
/// arrives in small chunks, so only a fraction is downloaded when
/// the decoder initializes. Duration must still reflect the full track.
#[kithara::test(tokio, timeout(Duration::from_secs(15)))]
#[case::throttled_no_hint(None)]
#[case::throttled_with_hint(Some("mp3"))]
async fn mp3_duration_correct_before_decode(#[case] hint: Option<&str>) {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(Consts::TEST_MP3_BYTES.to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Throttle {
            chunk: 10 * 1024,
            delay_ms: 50,
        },
    });
    let url = handle.url();
    let file_config = FileConfig::for_src(url.clone().into())
        .store(
            AssetStoreBuilder::default()
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let config = AudioConfig::<File>::for_stream(file_config)
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .maybe_hint(hint.map(String::from))
        .build();
    let audio = Audio::<Stream<File>>::new(config)
        .await
        .unwrap_or_else(|e| panic!("creation failed for url={url} hint={hint:?}: {e}"));

    let duration = audio.duration();
    assert!(
        duration.is_some(),
        "url={url} hint={hint:?}: duration must be available immediately (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        (dur_secs - Consts::TEST_MP3_DURATION_SECS).abs() < 2.0,
        "url={url} hint={hint:?}: expected ~{}s immediately, got {dur_secs:.1}s",
        Consts::TEST_MP3_DURATION_SECS
    );
}

#[kithara::test(tokio)]
async fn audio_file_extensionless_mp3_without_hint_uses_native_probe() {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(Consts::TEST_MP3_BYTES.to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let file_config = FileConfig::for_src(handle.url().into())
        .store(
            AssetStoreBuilder::default()
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let config = AudioConfig::<File>::new(
        file_config,
        kithara::bufpool::BytePool::default(),
        kithara::bufpool::PcmPool::default(),
    );
    let mut audio = Audio::<Stream<File>>::new(config).await.unwrap();

    let (samples_read, position, eof) = spawn_blocking(move || {
        let mut total = 0usize;
        let mut buf = [0.0f32; 4096];
        let mut saw_eof = false;

        for _ in 0..600 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => break,
                Ok(ReadOutcome::Frames { count, .. }) => {
                    total += count.get();
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Err(e) => panic!("decode error: {e}"),
            }
            if audio.position() >= Duration::from_secs(2) {
                break;
            }
        }

        (total, audio.position(), saw_eof)
    })
    .await
    .unwrap();

    assert!(samples_read > 0, "no decoded samples");
    assert!(
        position >= Duration::from_secs(2),
        "extensionless playback ended too early: pos={position:?} eof={eof} samples={samples_read}"
    );
}
