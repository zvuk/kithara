use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ChunkOutcome, PcmRead},
    decode::DecoderBackend,
    events::{AudioEvent, Event, EventBus},
    file::{File, FileConfig},
    platform::time::{self, Duration},
    stream::Stream,
};
use kithara_integration_tests::{TestServerHelper, TestTempDir, temp_dir};

#[kithara::fixture]
async fn server() -> TestServerHelper {
    TestServerHelper::new().await
}

/// Open a remote test.mp3 as `Audio<Stream<File>>` with optional hw/sw backend
/// and optional event bus. Centralises the setup shared by every seek test.
async fn open_test_mp3(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    backend: DecoderBackend,
    events: Option<EventBus>,
) -> Audio<Stream<File>> {
    let url = server.asset("test.mp3");
    let file_config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let config = AudioConfig::<File>::for_stream(file_config)
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .hint(String::from("mp3"))
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_events(events)
        .build();
    Audio::<Stream<File>>::new(config).await.unwrap()
}

/// Nonblocking re-poll loop: these tests are browser-portable (async body,
/// shared current-thread runtime), so the consumer must never park the
/// runtime thread — `block_on_underrun` is deliberately NOT used here.
/// A genuine stall is caught by the per-test timeout, not a hand-rolled
/// deadline. `flash(true)` keeps the re-poll sleep on the virtual clock
/// when called from a flash test.
#[kithara::flash(true)]
async fn next_chunk(audio: &mut Audio<Stream<File>>, stage: &str) {
    loop {
        match PcmRead::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(_)) => return,
            Ok(ChunkOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for {stage}");
            }
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("decode error at {stage}: {e}"),
        }

        audio.preload().expect("preload must succeed");
        time::sleep(Duration::from_millis(10)).await;
    }
}

/// Create Decoder<Stream<File>> and verify spec.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_creates_successfully(
    #[future] server: TestServerHelper,
    temp_dir: TestTempDir,
) {
    let server = server.await;
    let decoder = open_test_mp3(&server, &temp_dir, DecoderBackend::Symphonia, None).await;

    let spec = decoder.spec();
    assert!(spec.sample_rate.get() > 0);
    assert!(spec.channels > 0);
}

/// Decoder<Stream<File>> reads MP3 samples (no seek, just read).
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_reads_samples(#[future] server: TestServerHelper, temp_dir: TestTempDir) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, DecoderBackend::Symphonia, None).await;

    next_chunk(&mut decoder, "initial read").await;
}

/// Decoder<Stream<File>> seeks to a single target and resumes decoding.
///
/// Covers three zero-warmup scenarios: seek to 0, seek forward (2s), seek
/// to 0 again. The backward-seek case (which needs a warmup prelude) is a
/// separate test.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::to_zero(Duration::from_secs(0))]
#[case::forward(Duration::from_secs(2))]
async fn decoder_file_single_seek(
    #[future] server: TestServerHelper,
    temp_dir: TestTempDir,
    #[case] target: Duration,
) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, DecoderBackend::Symphonia, None).await;

    let spec = decoder.spec();
    assert!(spec.sample_rate.get() > 0 && spec.channels > 0);

    next_chunk(&mut decoder, "before seek").await;

    decoder.seek(target).unwrap();

    next_chunk(&mut decoder, "after seek").await;

    let spec_after = decoder.spec();
    assert_eq!(spec.sample_rate, spec_after.sample_rate);
    assert_eq!(spec.channels, spec_after.channels);
}

/// Decoder<Stream<File>> can seek backward to the beginning after a warmup.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_seek_backward(#[future] server: TestServerHelper, temp_dir: TestTempDir) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, DecoderBackend::Symphonia, None).await;

    for stage in 0..3 {
        next_chunk(&mut decoder, &format!("warmup chunk {stage}")).await;
    }

    decoder.seek(Duration::from_secs(3)).unwrap();
    next_chunk(&mut decoder, "after seek forward").await;

    decoder.seek(Duration::from_secs(0)).unwrap();
    next_chunk(&mut decoder, "after seek backward").await;
}

/// Decoder<Stream<File>> multiple seeks in sequence.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::sw(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw(DecoderBackend::Apple)
)]
async fn decoder_file_seek_multiple(
    #[future] server: TestServerHelper,
    temp_dir: TestTempDir,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, backend, None).await;

    next_chunk(&mut decoder, "initial read").await;

    for target in [1, 5, 2, 0] {
        decoder.seek(Duration::from_secs(target)).unwrap();
        next_chunk(&mut decoder, &format!("after seek to {target}s")).await;
    }
}

/// Decoder<Stream<File>> events are emitted on seek.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_seek_emits_events(#[future] server: TestServerHelper, temp_dir: TestTempDir) {
    let server = server.await;
    let bus = EventBus::new(64);
    let mut events_rx = bus.subscribe();

    let mut decoder = open_test_mp3(&server, &temp_dir, DecoderBackend::Symphonia, Some(bus)).await;

    next_chunk(&mut decoder, "before seek events").await;
    decoder.seek(Duration::from_secs(2)).unwrap();

    let mut got_format = false;
    let mut got_seek = false;
    let mut buf = [0.0_f32; 1024];

    loop {
        while let Ok(ev) = events_rx.try_recv().map(|env| env.event) {
            match ev {
                Event::Audio(AudioEvent::FormatDetected { .. }) => {
                    got_format = true;
                }
                Event::Audio(AudioEvent::SeekComplete { .. }) => {
                    got_seek = true;
                }
                _ => {}
            }
        }

        if got_format && got_seek {
            break;
        }

        use kithara::audio::ReadOutcome;
        match decoder.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                decoder.preload().expect("preload must succeed");
                time::sleep(Duration::from_millis(10)).await;
            }
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for seek events");
            }
            Err(e) => panic!("decode error: {e}"),
        }
    }

    assert!(got_format, "should have received FormatDetected event");
    assert!(got_seek, "should have received SeekComplete event");
}
