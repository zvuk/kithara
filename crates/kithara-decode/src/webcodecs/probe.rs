use kithara_platform::sync::OnceLock;
use kithara_stream::AudioCodec;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioDecoder, AudioDecoderConfig, AudioDecoderSupport};

use super::codec::codec_string;

static SUPPORT: OnceLock<Support> = OnceLock::new();

#[derive(Clone, Copy, Default)]
struct Support {
    aac_lc: bool,
    aac_he: bool,
    aac_he_v2: bool,
    mp3: bool,
    flac: bool,
}

impl Support {
    const CODECS: [AudioCodec; 5] = [
        AudioCodec::AacLc,
        AudioCodec::AacHe,
        AudioCodec::AacHeV2,
        AudioCodec::Mp3,
        AudioCodec::Flac,
    ];

    fn set(&mut self, codec: AudioCodec, supported: bool) {
        match codec {
            AudioCodec::AacLc => self.aac_lc = supported,
            AudioCodec::AacHe => self.aac_he = supported,
            AudioCodec::AacHeV2 => self.aac_he_v2 = supported,
            AudioCodec::Mp3 => self.mp3 = supported,
            AudioCodec::Flac => self.flac = supported,
            _ => {}
        }
    }

    fn supports(self, codec: AudioCodec) -> bool {
        match codec {
            AudioCodec::AacLc => self.aac_lc,
            AudioCodec::AacHe => self.aac_he,
            AudioCodec::AacHeV2 => self.aac_he_v2,
            AudioCodec::Mp3 => self.mp3,
            AudioCodec::Flac => self.flac,
            _ => false,
        }
    }
}

/// Start the main-thread WebCodecs capability probe.
#[doc(hidden)]
pub fn spawn_webcodecs_probe() {
    drop(kithara_platform::tokio::task::spawn(async {
        let mut support = Support::default();
        for codec in Support::CODECS {
            support.set(codec, probe(codec).await);
        }
        if SUPPORT.set(support).is_err() {
            tracing::debug!("WebCodecs capability snapshot was already published");
        }
    }));
}

pub(crate) fn supported(codec: AudioCodec) -> bool {
    SUPPORT.get().is_some_and(|support| support.supports(codec))
}

async fn probe(codec: AudioCodec) -> bool {
    let Some(codec_string) = codec_string(codec) else {
        return false;
    };
    let config = AudioDecoderConfig::new(codec_string, 2, 44_100);
    let promise = AudioDecoder::is_config_supported(&config);
    let supported = match JsFuture::from(promise).await {
        Ok(value) => value
            .dyn_into::<AudioDecoderSupport>()
            .ok()
            .and_then(|support| support.get_supported())
            .unwrap_or(false),
        Err(error) => {
            tracing::warn!(?codec, ?error, "WebCodecs capability probe failed");
            false
        }
    };
    tracing::debug!(?codec, supported, "probed WebCodecs capability");
    supported
}
