use js_sys::Uint8Array;
use kithara_platform::sync::{OnceLock, mpsc};
use kithara_stream::AudioCodec;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioDecoder, AudioDecoderConfig, AudioDecoderSupport};

use super::{codec::codec_string, host::spawn_host, protocol::HostCmd};

fn host_cmd() -> &'static OnceLock<mpsc::Sender<HostCmd>> {
    static HOST_CMD: OnceLock<mpsc::Sender<HostCmd>> = OnceLock::new();
    &HOST_CMD
}

fn support() -> &'static OnceLock<Support> {
    static SUPPORT: OnceLock<Support> = OnceLock::new();
    &SUPPORT
}

#[derive(Clone, Copy, Default)]
struct Support {
    aac_lc: bool,
    aac_he: bool,
    aac_he_v2: bool,
    mp3: bool,
    flac: bool,
}

impl Support {
    /// Minimal 44.1 kHz, stereo, 16-bit FLAC STREAMINFO for capability probing.
    const FLAC_PROBE_STREAMINFO: [u8; 34] = [
        0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // sample rate (20b) | channels - 1 (3b) | bits per sample - 1 (5b) | samples (36b)
        0x0A, 0xC4, 0x42, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

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

/// Initialize the main-thread WebCodecs runtime and capability probe.
#[doc(hidden)]
pub fn spawn_webcodecs_probe() {
    if host_cmd().get().is_some() {
        tracing::debug!("WebCodecs host was already initialized");
        return;
    }
    let _ = host_cmd().set(spawn_host());
    drop(kithara_platform::tokio::task::spawn(async {
        let mut snapshot = Support::default();
        for codec in Support::CODECS {
            snapshot.set(codec, probe(codec).await);
        }
        if support().set(snapshot).is_err() {
            tracing::debug!("WebCodecs capability snapshot was already published");
        }
    }));
}

pub(crate) fn host_sender() -> Option<mpsc::Sender<HostCmd>> {
    host_cmd().get().cloned()
}

pub(crate) fn supported(codec: AudioCodec) -> bool {
    support()
        .get()
        .is_some_and(|support| support.supports(codec))
}

async fn probe(codec: AudioCodec) -> bool {
    let Some(codec_string) = codec_string(codec) else {
        return false;
    };
    let config = AudioDecoderConfig::new(codec_string, 2, 44_100);
    if let Some(description) = probe_description(codec) {
        config.set_description(&Uint8Array::from(description.as_slice()));
    }
    let promise = AudioDecoder::is_config_supported(&config);
    let supported = match JsFuture::from(promise).await {
        // The promise resolves to an AudioDecoderSupport DICTIONARY (a plain
        // JS object with no prototype), so instanceof-based `dyn_into` always
        // fails; cast unchecked and read the member.
        Ok(value) => value
            .unchecked_into::<AudioDecoderSupport>()
            .get_supported()
            .unwrap_or(false),
        Err(error) => {
            tracing::warn!(?codec, ?error, "WebCodecs capability probe failed");
            false
        }
    };
    tracing::debug!(?codec, supported, "probed WebCodecs capability");
    supported
}

fn probe_description(codec: AudioCodec) -> Option<Vec<u8>> {
    match codec {
        AudioCodec::Flac => {
            let mut description = Vec::with_capacity(4 + 4 + Support::FLAC_PROBE_STREAMINFO.len());
            description.extend_from_slice(b"fLaC");
            description.extend_from_slice(&[0x80, 0, 0, 34]);
            description.extend_from_slice(&Support::FLAC_PROBE_STREAMINFO);
            Some(description)
        }
        _ => None,
    }
}
