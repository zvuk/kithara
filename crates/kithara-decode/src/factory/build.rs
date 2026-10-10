use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;

use crate::{
    DecodeResult, Decoder, DecoderConfig,
    codec::FrameCodec,
    composed::{ComposedDecoder, DecoderRuntime},
    demuxer::Demuxer,
};

pub(super) fn finish<D, C>(
    demuxer: D,
    codec: C,
    config: DecoderConfig<
        impl ResamplerBackend,
        impl HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    >,
) -> DecodeResult<Box<dyn Decoder>>
where
    D: Demuxer + 'static,
    C: FrameCodec + 'static,
{
    let pools = config.pools;
    let decoder = ComposedDecoder::new(
        demuxer,
        codec,
        DecoderRuntime {
            pools: pools.clone(),
            byte_len_handle: config.byte_len_handle,
            hooks: config.hooks,
        },
    );
    crate::resampled::wrap(Box::new(decoder), config.resampler, &pools)
}

#[cfg(feature = "ape")]
pub(super) fn create_ape<B, S>(
    source: crate::traits::BoxedSource,
    config: DecoderConfig<B, S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let pools = config.pools.clone();
    let resampler = config.resampler;
    let decoder = crate::ape::ApeDecoder::open(
        source,
        DecoderRuntime {
            pools: pools.clone(),
            hooks: config.hooks,
            byte_len_handle: config.byte_len_handle,
        },
    )?;
    crate::resampled::wrap(Box::new(decoder), resampler, &pools)
}
