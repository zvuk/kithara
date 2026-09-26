use std::sync::OnceLock;

use symphonia::{
    core::{codecs::registry::CodecRegistry, formats::probe::Probe},
    default::{
        formats::{
            AdtsReader, AiffReader, CafReader, FlacReader, IsoMp4Reader, MkvReader, OggReader,
            WavReader,
        },
        meta::{ApeReader, Id3v1Reader, Id3v2Reader},
        register_enabled_codecs,
    },
};

pub(crate) fn get_codecs() -> &'static CodecRegistry {
    static CODEC_REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();

    CODEC_REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        register_enabled_codecs(&mut registry);
        #[cfg(feature = "fdk-aac")]
        registry.register_audio_decoder::<crate::symphonia::fdk::AacDecoder>();
        registry
    })
}

/// Formats and metadata this crate probes for.
///
/// Built explicitly instead of through `register_enabled_formats` so
/// [`kithara_mpa::MpaReader`] is the only MPEG-audio candidate. Symphonia
/// registers its own reader in the same tier, and a tier is resolved by the
/// first candidate whose marker matches and whose score accepts.
///
/// That makes the order load-bearing — ADTS and MPEG audio share a `0xFF`
/// sync prefix, so ADTS must stay ahead of MPA. This list is Symphonia's own
/// registration order with its MPA reader replaced in place, so probing
/// resolves exactly as it did before the fork was introduced.
pub(crate) fn get_probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();

    PROBE.get_or_init(|| {
        let mut probe = Probe::new();

        probe.register_format::<AdtsReader<'_>>();
        probe.register_format::<CafReader<'_>>();
        probe.register_format::<FlacReader<'_>>();
        probe.register_format::<IsoMp4Reader<'_>>();
        probe.register_format::<kithara_mpa::MpaReader<'_>>();
        probe.register_format::<AiffReader<'_>>();
        probe.register_format::<WavReader<'_>>();
        probe.register_format::<OggReader<'_>>();
        probe.register_format::<MkvReader<'_>>();

        probe.register_metadata::<ApeReader<'_>>();
        probe.register_metadata::<Id3v1Reader<'_>>();
        probe.register_metadata::<Id3v2Reader<'_>>();

        probe
    })
}
