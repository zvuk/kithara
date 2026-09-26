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

#[kithara::fixture]
pub async fn mixed_encrypted() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = mixed_codec_ladder_url(&server, true).await;
    (server, url)
}
