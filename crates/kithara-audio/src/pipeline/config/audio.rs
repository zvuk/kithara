use std::num::NonZeroU32;

use kithara_config::Config;
use kithara_derive::Patch;
use kithara_events::EventBus;
use kithara_platform::CancelToken;
use kithara_resampler::{NoResamplerBackend, ResamplerBackend};
use kithara_stream::{MediaInfo, StreamType};

use crate::{
    pipeline::config::{AudioDecoderConfig, AudioDecoderConfigPatch},
    traits::AudioObserver,
};

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration. Holds
/// the pipeline's own tunables beside the per-call wiring a caller hands over:
/// the stream configuration, the event bus, the cancel token, the format hints
/// and the optional PCM observer.
///
/// [`AudioConfigPatch`] is what a configuration document may say about it.
#[derive(Config, Patch)]
#[config(construction, builder(start_fn = for_stream))]
#[non_exhaustive]
pub struct AudioConfig<T: StreamType, B = NoResamplerBackend> {
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    #[config(
        skip = "transferred to the stream",
        builder(start_fn),
        patch(skip),
        get(ref)
    )]
    pub(crate) stream: T::Config,
    /// Target sample rate of the audio host (for resampling). Not a document
    /// key: this is the rate the audio host actually opened, and the
    /// resource-preparation step that shares a player's engine always
    /// overwrites it with the engine's master or configured rate. A document
    /// value would be overwritten by the first host that disagrees with it.
    #[config(
        skip = "transferred to the host sample-rate owner",
        patch(skip),
        get(copy)
    )]
    pub host_sample_rate: Option<NonZeroU32>,
    /// Decoder construction settings, including decoder-side resampling. A
    /// document names it under `audio.decoder`.
    #[config(
        skip = "transferred to decoder dependencies",
        builder(default),
        patch(nested),
        get(ref)
    )]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[config(skip = "transferred to the event bus", builder(name = events), patch(skip))]
    pub(crate) bus: Option<EventBus>,
    /// Master cancel token for the audio pipeline.
    #[config(skip = "composed into the audio cancel scope", patch(skip))]
    pub(crate) cancel: Option<CancelToken>,
    /// Optional format hint (file extension like "mp3", "wav")
    #[config(skip = "transferred to decoder construction", patch(skip))]
    pub(crate) hint: Option<String>,
    /// Media info hint for format detection
    #[config(skip = "transferred to decoder construction", patch(skip))]
    pub(crate) media_info: Option<MediaInfo>,
    /// Optional bounded, nonblocking observer of decoder-output PCM.
    /// [`kithara_signal::AudioChunk::meta`] describes its post-conversion format;
    /// it runs before playback effects and owns any asynchronous copy.
    #[config(skip = "transferred to the decoded source", patch(skip))]
    pub(crate) observer: Option<Box<dyn AudioObserver>>,
}

impl<T, B> AudioConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    /// Return the configured event bus.
    #[must_use]
    pub const fn bus(&self) -> Option<&EventBus> {
        self.bus.as_ref()
    }

    /// Return the configured cancellation token.
    #[must_use]
    pub const fn cancel(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Return the optional format hint.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Return the media information hint.
    #[must_use]
    pub const fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_decode::{DecoderBackend, GaplessMode};
    use kithara_test_utils::kithara;

    use super::AudioConfigPatch;

    /// The decoder is a section of its own, and the backend object it
    /// resamples through is not one of its keys.
    #[kithara::test(native, flash(false))]
    fn the_decoder_section_names_the_backend_and_the_gapless_mode() {
        let backend = DecoderBackend::default();
        let patch: AudioConfigPatch = serde_yaml_ng::from_str(&format!(
            "decoder:\n  backend: {backend}\n  gapless_mode:\n    mode: disabled\n",
        ))
        .expect("the document types");

        assert_eq!(patch.decoder.backend, Some(backend));
        assert_eq!(patch.decoder.gapless_mode, Some(GaplessMode::Disabled));
    }

    /// The decoder-side resampler carries the backend object the construction
    /// site hands over (see the field's doc comment).
    #[kithara::test(native, flash(false))]
    fn the_decoder_resampler_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<AudioConfigPatch>("decoder:\n  resampler: {}\n")
            .expect_err("a document cannot hand over a resampler backend");

        assert!(error.to_string().contains("resampler"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_decoder_backend_this_build_cannot_provide_is_refused_by_name() {
        let error = serde_yaml_ng::from_str::<AudioConfigPatch>("decoder:\n  backend: teapot\n")
            .expect_err("a backend nothing implements must not pass silently");

        assert!(error.to_string().contains("teapot"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<AudioConfigPatch>("headroom: 8\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("headroom"), "{error}");
    }

    /// The deleted consumer policy cannot be revived by a configuration document.
    #[kithara::test(native, flash(false))]
    fn the_realtime_unsafe_wake_mode_is_not_a_document_key() {
        let error =
            serde_yaml_ng::from_str::<AudioConfigPatch>("consumer_wake_mode: immediate_off_rt\n")
                .expect_err(
                    "a capability that moves reads onto the render callback is not \
                     document-settable",
                );

        assert!(error.to_string().contains("consumer_wake_mode"), "{error}");
    }

    /// `block_on_underrun` can park a real-time audio callback (see the
    /// field's doc comment).
    #[kithara::test(native, flash(false))]
    fn the_realtime_unsafe_block_on_underrun_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<AudioConfigPatch>("block_on_underrun: true\n")
            .expect_err("a field that can park the audio callback must not be document-settable");

        assert!(error.to_string().contains("block_on_underrun"), "{error}");
    }

    /// `host_sample_rate` is the rate the audio host actually opened (see the
    /// field's doc comment).
    #[kithara::test(native, flash(false))]
    fn the_runtime_owned_host_sample_rate_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<AudioConfigPatch>("host_sample_rate: 48000\n")
            .expect_err("the audio host owns its own rate");

        assert!(error.to_string().contains("host_sample_rate"), "{error}");
    }
}
