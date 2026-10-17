use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{AudioCodec, ByteMap, ContainerFormat, needs_exact_byte_sizes};

use super::build::finish;
use crate::{DecodeResult, Decoder, DecoderConfig, traits::BoxedSource};

/// Gate for the segment-aware fMP4 path. Routes AAC / FLAC fMP4 with a
/// surfaced `SegmentedSource` (HLS) through `Fmp4SegmentDecoder`. File
/// sources without segment metadata fall through to the legacy
/// `IsoMp4Reader` path.
pub(super) fn should_use_segment_aware<S>(
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig<impl ResamplerBackend, S>,
) -> bool {
    segment_aware_container(codec, container) && config.byte_map.is_some()
}

/// Codec+container half of the segment-aware fMP4 decision, factored out of
/// [`should_use_segment_aware`] so [`crate::DecoderFactory::reader_profile`] can
/// ask the same question without a [`DecoderConfig`]. AAC / FLAC in fMP4 is
/// the only segment-aware path; everything else reads incrementally.
pub(super) const fn segment_aware_container(
    codec: AudioCodec,
    container: Option<ContainerFormat>,
) -> bool {
    !needs_exact_byte_sizes(Some(codec), container)
}

/// Generic builder for the segment-aware fMP4 path. Owns the
/// [`crate::fmp4::Fmp4SegmentDemuxer`] open + pool-resolution + [`crate::composed::ComposedDecoder`]
/// boilerplate so apple/android/symphonia call-sites collapse into a
/// single closure that opens the codec from `TrackInfo`.
pub(super) fn build_fmp4_segment_decoder<C>(
    source: BoxedSource,
    layout: Arc<dyn ByteMap>,
    mut config: DecoderConfig<
        impl ResamplerBackend,
        impl HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    >,
    open_codec: impl FnOnce(&crate::demuxer::TrackInfo) -> DecodeResult<C>,
) -> DecodeResult<Box<dyn Decoder>>
where
    C: crate::codec::FrameCodec + 'static,
{
    use crate::{demuxer::Demuxer, fmp4::Fmp4SegmentDemuxer};

    let demuxer = Fmp4SegmentDemuxer::open(
        source,
        layout,
        config.pools.clone(),
        config.construction_gate.take(),
    )?;
    let codec = open_codec(demuxer.track_info())?;
    finish(demuxer, codec, config)
}
