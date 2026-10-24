use std::{
    collections::BTreeMap,
    mem,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{
        AudioConfig, AudioControl, AudioEvent, AudioObserveError, AudioObserver, AudioRead,
        ChunkOutcome, ReadOutcome,
    },
    decode::DecoderBackend,
    events::EventBus,
    file::{File, FileConfig},
    platform::time::{self, Duration},
    play::{PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend},
    signal::{AudioChunk, AudioChunkInfo},
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    event::TestEvent,
    hls_fixture::create_test_downloader,
    mock::LaneAudio,
};
use kithara_test_fixtures::SignalAsset;
use kithara_test_utils::{TestTempDir, temp_dir, wait_until};
use url::Url;

async fn source(asset: SignalAsset) -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(asset);
    (server, url)
}

/// Open a remote encoded asset as `Audio<Stream<File>>` with optional hw/sw backend
/// and optional event bus. Centralises the setup shared by every seek test.
async fn open_test_audio(
    asset: SignalAsset,
    url: &Url,
    temp_dir: &TestTempDir,
    backend: DecoderBackend,
    events: Option<EventBus>,
) -> Option<LaneAudio<Stream<File<TestPools>>, TestPools>> {
    let pools = pools();
    let file_config = FileConfig::for_src(url.clone().into())
        .downloader(create_test_downloader())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().into(),
                })
                .build(),
        )
        .build();
    let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .hint(url.path().rsplit_once('.').unwrap().1.to_owned())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .maybe_events(events)
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    kithara_integration_tests::fixtures::assert_fixture_open(
        asset,
        kithara_integration_tests::mock::load_audio(&worker, config).await,
    )
}

/// Nonblocking re-poll loop: these tests are browser-portable (async body,
/// shared current-thread runtime), so the consumer must never park the
/// runtime thread — `block_on_underrun` is deliberately NOT used here.
/// A genuine stall is caught by the per-test timeout, not a hand-rolled
/// deadline. `flash(true)` keeps the re-poll sleep on the virtual clock
/// when called from a flash test.
#[kithara::flash(true)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn next_chunk(audio: &mut LaneAudio<Stream<File<TestPools>>, TestPools>, stage: &str) {
    loop {
        hang_tick!();
        match AudioRead::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(_)) => {
                hang_reset!();
                return;
            }
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

/// Decoder<Stream<File>> reads encoded samples (no seek, just read).
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[case(SignalAsset::MP3_TRACK_SINE440_187S)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3)
)]
#[cfg_attr(not(target_arch = "wasm32"), case(SignalAsset::PROFILE_TAGGED_MP3_ID3))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3)
)]
async fn decoder_file_reads_samples(#[case] asset: SignalAsset, temp_dir: TestTempDir) {
    let (_server, url) = source(asset).await;
    let Some(mut decoder) =
        open_test_audio(asset, &url, &temp_dir, DecoderBackend::default(), None).await
    else {
        return;
    };

    next_chunk(&mut decoder, "initial read").await;
}

/// Decoder<Stream<File>> seeks to a single target and resumes decoding.
///
/// Covers three zero-warmup scenarios: seek to 0, seek forward (2s), seek
/// to 0 again. The backward-seek case (which needs a warmup prelude) is a
/// separate test.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[case(SignalAsset::MP3_TRACK_SINE440_187S, Duration::from_secs(0))]
#[case(SignalAsset::MP3_TRACK_SINE440_187S, Duration::from_secs(2))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT,
        Duration::from_secs(0)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT,
        Duration::from_secs(2)
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_MP3_ID3, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_MP3_ID3, Duration::from_secs(2))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3, Duration::from_secs(0))
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3, Duration::from_secs(2))
)]
async fn decoder_file_single_seek(
    #[case] asset: SignalAsset,
    temp_dir: TestTempDir,
    #[case] target: Duration,
) {
    let (_server, url) = source(asset).await;
    let Some(mut decoder) =
        open_test_audio(asset, &url, &temp_dir, DecoderBackend::default(), None).await
    else {
        return;
    };

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
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[case(SignalAsset::MP3_TRACK_SINE440_187S)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3)
)]
#[cfg_attr(not(target_arch = "wasm32"), case(SignalAsset::PROFILE_TAGGED_MP3_ID3))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3)
)]
async fn decoder_file_seek_backward(#[case] asset: SignalAsset, temp_dir: TestTempDir) {
    let (_server, url) = source(asset).await;
    let Some(mut decoder) =
        open_test_audio(asset, &url, &temp_dir, DecoderBackend::default(), None).await
    else {
        return;
    };

    for stage in 0..3 {
        next_chunk(&mut decoder, &format!("warmup chunk {stage}")).await;
    }

    decoder.seek(Duration::from_secs(3)).unwrap();
    next_chunk(&mut decoder, "after seek forward").await;

    decoder.seek(Duration::from_secs(0)).unwrap();
    next_chunk(&mut decoder, "after seek backward").await;
}

/// Decoder<Stream<File>> multiple seeks in sequence.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[case(SignalAsset::MP3_TRACK_SINE440_187S, DecoderBackend::default())]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::MP3_TRACK_SINE440_187S, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::MP3_TRACK_SINE440_187S, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH, DecoderBackend::default())
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH, DecoderBackend::default())
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH, DecoderBackend::default())
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(
        SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(
        SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT,
        DecoderBackend::default()
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT,
        DecoderBackend::Symphonia
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(
        SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT,
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3, DecoderBackend::default())
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_MP3_ID3, DecoderBackend::default())
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::PROFILE_TAGGED_MP3_ID3, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_TAGGED_MP3_ID3, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3, DecoderBackend::default())
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3, DecoderBackend::Apple)
)]
async fn decoder_file_seek_multiple(
    #[case] asset: SignalAsset,
    temp_dir: TestTempDir,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_server, url) = source(asset).await;
    let Some(mut decoder) = open_test_audio(asset, &url, &temp_dir, backend, None).await else {
        return;
    };

    next_chunk(&mut decoder, "initial read").await;

    for target in [1, 5, 2, 0] {
        decoder.seek(Duration::from_secs(target)).unwrap();
        next_chunk(&mut decoder, &format!("after seek to {target}s")).await;
    }
}

/// Decoder<Stream<File>> events are emitted on seek.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[case(SignalAsset::MP3_TRACK_SINE440_187S)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_AAC_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_FLAC_ID3)
)]
#[cfg_attr(not(target_arch = "wasm32"), case(SignalAsset::PROFILE_TAGGED_MP3_ID3))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3)
)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn decoder_file_seek_emits_events(#[case] asset: SignalAsset, temp_dir: TestTempDir) {
    let (_server, url) = source(asset).await;
    let bus = EventBus::new(64);
    let mut events_rx = bus.subscribe();

    let Some(mut decoder) =
        open_test_audio(asset, &url, &temp_dir, DecoderBackend::default(), Some(bus)).await
    else {
        return;
    };

    next_chunk(&mut decoder, "before seek events").await;
    decoder.seek(Duration::from_secs(2)).unwrap();

    let mut got_format = false;
    let mut got_seek = false;
    let mut buf = [0.0_f32; 1024];

    loop {
        hang_tick!();
        while let Ok(ev) = events_rx.try_recv().map(|env| env.event) {
            hang_tick!();
            hang_reset!();
            match ev {
                TestEvent::Audio(AudioEvent::FormatDetected { .. }) => {
                    got_format = true;
                }
                TestEvent::Audio(AudioEvent::SeekComplete { .. }) => {
                    got_seek = true;
                }
                _ => {}
            }
        }

        if got_format && got_seek {
            break;
        }

        match decoder.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                decoder.preload().expect("preload must succeed");
                time::sleep(Duration::from_millis(10)).await;
            }
            Ok(ReadOutcome::Frames { .. }) => {
                hang_reset!();
            }
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for seek events");
            }
            Err(e) => panic!("decode error: {e}"),
        }
    }

    assert!(got_format, "should have received FormatDetected event");
    assert!(got_seek, "should have received SeekComplete event");
}

/// One decoded chunk as an observer saw it.
struct Decoded {
    meta: AudioChunkInfo,
    pcm: Vec<f32>,
}

/// Every decoded chunk an observer sees, keyed by its PCM identity.
#[derive(Default)]
struct Identities {
    seen: BTreeMap<(u64, u64), Decoded>,
    observed: usize,
    revisited: usize,
    conflicts: Vec<String>,
}

impl Identities {
    fn record(&mut self, chunk: &AudioChunk) {
        self.observed += 1;
        let key = (chunk.meta.source_revision, chunk.meta.frame_offset);
        let samples: &[f32] = chunk.as_ref();
        if let Some(decoded) = self.seen.get(&key) {
            let conflict = (decoded.meta != chunk.meta || decoded.pcm.as_slice() != samples)
                .then(|| format!("{key:?}: {:?} then {:?}", decoded.meta, chunk.meta));
            self.revisited += 1;
            self.conflicts.extend(conflict);
        } else {
            self.seen.insert(
                key,
                Decoded {
                    meta: chunk.meta,
                    pcm: samples.to_vec(),
                },
            );
        }
    }
}

struct IdentityObserver(Arc<Mutex<Identities>>);

impl AudioObserver for IdentityObserver {
    fn try_observe(&mut self, chunk: &AudioChunk) -> Result<(), AudioObserveError> {
        self.0.lock().expect("identity ledger").record(chunk);
        Ok(())
    }
}

#[kithara::flash(true)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn read_until_decoded(
    audio: &mut LaneAudio<Stream<File<TestPools>>, TestPools>,
    identities: &Mutex<Identities>,
    target: usize,
    stage: &str,
) {
    while identities.lock().expect("identity ledger").observed < target {
        hang_tick!();
        match AudioRead::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(_)) => {
                hang_reset!();
                continue;
            }
            Ok(ChunkOutcome::Eof { .. }) => panic!("unexpected EOF while reading {stage}"),
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("decode error at {stage}: {e}"),
        }
        audio.preload().expect("preload must succeed");
        time::sleep(Duration::from_millis(10)).await;
    }
}

/// A decoded chunk's `(source_revision, frame_offset)` names one PCM payload.
/// Seeking back onto chunk starts already decoded either reproduces those
/// chunks exactly under the same identity (native rate: the re-decode is
/// sample-exact) or decodes them under a new revision (decoder-side
/// resampling restarts its output clock on a fractional source phase).
#[kithara::test(tokio, native, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(SignalAsset::MP3_SAW_6S, DecoderBackend::Symphonia, None)
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        DecoderBackend::Symphonia,
        None
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::MP3_SAW_6S,
        DecoderBackend::Symphonia,
        NonZeroU32::new(48_000)
    )
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case(
        SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
        DecoderBackend::Symphonia,
        NonZeroU32::new(48_000)
    )
)]
#[case(
    SignalAsset::MP3_SAW_6S,
    DecoderBackend::default(),
    NonZeroU32::new(48_000)
)]
#[case(
    SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT,
    DecoderBackend::default(),
    NonZeroU32::new(48_000)
)]
async fn a_seek_onto_a_decoded_chunk_start_never_reuses_its_identity_for_other_pcm(
    #[case] asset: SignalAsset,
    #[case] backend: DecoderBackend,
    #[case] host_rate: Option<NonZeroU32>,
    temp_dir: TestTempDir,
) {
    let (_server, url) = source(asset).await;
    let identities = Arc::new(Mutex::new(Identities::default()));
    let pools = pools();
    let file_config = FileConfig::for_src(url.clone().into())
        .downloader(create_test_downloader())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().into(),
                })
                .build(),
        )
        .build();
    let config = AudioConfig::<File<TestPools>, PlaybackResamplerBackend>::for_stream(file_config)
        .hint(url.path().rsplit_once('.').unwrap().1.to_owned())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .observer(Box::new(IdentityObserver(identities.clone())))
        .maybe_host_sample_rate(host_rate)
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let Some(mut audio) = kithara_integration_tests::fixtures::assert_fixture_open(
        asset,
        kithara_integration_tests::mock::load_audio(&worker, config).await,
    ) else {
        return;
    };

    read_until_decoded(&mut audio, &identities, 60, "the linear pass").await;
    let starts: Vec<(u64, u32)> = identities
        .lock()
        .expect("identity ledger")
        .seen
        .values()
        .skip(5)
        .step_by(7)
        .take(5)
        .map(|decoded| {
            (
                decoded.meta.frame_offset,
                decoded.meta.spec.sample_rate.get(),
            )
        })
        .collect();
    assert_eq!(
        starts.len(),
        5,
        "the linear pass must decode the seek targets"
    );
    let mut seek_revisits = Vec::new();
    for (frame, rate) in &starts {
        let before = identities.lock().expect("identity ledger").revisited;
        let nanos = (u128::from(*frame) * 1_000_000_000).div_ceil(u128::from(*rate));
        let target = Duration::from_nanos(u64::try_from(nanos).expect("seek target"));
        audio.seek(target).expect("seek onto a decoded chunk start");
        wait_until(Duration::from_secs(1), "the seek segment is ready", || {
            audio.current_segment_ready()
        })
        .await
        .expect("seek segment preparation");
        let target = identities.lock().expect("identity ledger").observed + 4;
        read_until_decoded(
            &mut audio,
            &identities,
            target,
            &format!("after the seek to frame {frame}"),
        )
        .await;
        let observed = identities.lock().expect("identity ledger");
        seek_revisits.push((*frame, before, observed.revisited));
    }

    let identities = mem::take(&mut *identities.lock().expect("identity ledger"));
    assert!(
        identities.conflicts.is_empty(),
        "one identity named different PCM: {:#?}",
        identities.conflicts
    );
    if host_rate.is_none() {
        assert!(
            identities.revisited >= starts.len(),
            "a native-rate seek must reproduce decoded chunks under their identity; revisited {}; seeks {seek_revisits:?}",
            identities.revisited,
        );
    }
}
