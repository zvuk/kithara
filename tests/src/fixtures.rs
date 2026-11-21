#[cfg(not(target_arch = "wasm32"))]
use futures::executor::block_on;
use kithara;
use kithara_test_fixtures::SignalAsset;
use url::Url;

use crate::{TestServerHelper, mixed_codec_ladder_url};

#[kithara::fixture]
pub async fn served_mp3() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE880_48K_162S);
    (server, url)
}

#[kithara::fixture]
pub async fn served_short_mp3() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE880_30S);
    (server, url)
}

#[kithara::fixture]
pub async fn served_silence() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SILENCE_1S);
    (server, url)
}

#[kithara::fixture]
pub async fn served_aac() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::AAC_SINE440_60S_320K);
    (server, url)
}

#[kithara::fixture]
pub async fn mixed_plain() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = mixed_codec_ladder_url(&server, false).await;
    (server, url)
}

/// Master URL of [`mixed_plain`]'s ladder, served before the test body runs.
///
/// Native registration reads every ladder segment from the fixture store and
/// never suspends, so a `#[by_ref]` parameter keeps that read out of the
/// test's time budget. The ladder stays registered in the process-global
/// server.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn mixed_plain_url() -> Url {
    block_on(async {
        let server = TestServerHelper::new().await;
        mixed_codec_ladder_url(&server, false).await
    })
}

#[kithara::fixture]
pub async fn mixed_encrypted() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = mixed_codec_ladder_url(&server, true).await;
    (server, url)
}

/// Exact native rejection expected for fixtures outside Android's composition.
/// ALAC is optional: a device providing its `MediaCodec` must decode it instead.
pub fn android_fixture_rejection(asset: SignalAsset) -> Option<(&'static str, bool)> {
    if !cfg!(target_os = "android") {
        return None;
    }
    match asset {
        SignalAsset::PROFILE_OGG_VORBIS_44100_2CH => Some(("Unsupported codec: Vorbis", false)),
        SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH => Some(("Unsupported codec: Opus", false)),
        SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT => {
            Some(("Unsupported codec: Pcm", false))
        }
        SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT => {
            Some(("Unsupported codec: Ape", false))
        }
        SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT | SignalAsset::PROFILE_ALAC_SILENCE_TAIL => {
            Some((
                "Decoder error: android backend failed during codec-create-decoder: mime=audio/alac returned null",
                true,
            ))
        }
        _ => None,
    }
}

/// Validate fixture admission before running the existing decoding assertions.
/// A supported fixture must open; a rejected fixture must report its exact cause.
pub fn assert_fixture_open<T, E: std::fmt::Display>(
    asset: SignalAsset,
    result: Result<T, E>,
) -> Option<T> {
    match (android_fixture_rejection(asset), result) {
        (Some((expected, _)), Err(error)) => {
            assert_eq!(error.to_string(), expected, "{} rejection", asset.name());
            None
        }
        (Some((_, false)), Ok(_)) => panic!(
            "{} must be rejected by MediaCodec-only composition",
            asset.name()
        ),
        (_, Ok(value)) => Some(value),
        (_, Err(error)) => panic!("{} must decode: {error}", asset.name()),
    }
}
