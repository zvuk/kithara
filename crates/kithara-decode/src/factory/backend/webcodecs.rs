use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{AudioCodec, ContainerFormat};

use super::super::segment::{build_fmp4_segment_decoder, should_use_segment_aware};
use crate::{DecodeResult, Decoder, DecoderConfig, traits::BoxedSource};

pub(in crate::factory) fn create<B, S>(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig<B, S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    use crate::webcodecs::codec::{WebCodecsCodec, supports as webcodecs_supports};

    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
    {
        if webcodecs_supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware WebCodecs path"
            );
            let gapless = config.gapless;
            let pools = config.pools.clone();
            return build_fmp4_segment_decoder(source, layout, config, move |track| {
                WebCodecsCodec::open(track, gapless, pools.clone())
            });
        }
        return super::software::create_segment(source, codec, layout, config);
    }

    standalone::create(source, codec, container, config)
}

#[cfg(feature = "symphonia")]
mod standalone {
    use super::*;
    use crate::factory::build::finish;

    pub(super) fn create<B, S>(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        use crate::{demuxer::Demuxer, webcodecs::codec::WebCodecsCodec};

        if !crate::webcodecs::codec::supports(codec) {
            return super::super::software::create(source, codec, container, config);
        }

        tracing::debug!(
            ?codec,
            ?container,
            "file-symphonia: dispatching to ComposedDecoder<SymphoniaDemuxer, WebCodecsCodec>"
        );
        let demuxer =
            crate::factory::software::open_file_demuxer(source, codec, container, &config)?;
        let codec_impl =
            WebCodecsCodec::open(demuxer.track_info(), config.gapless, config.pools.clone())?;
        finish(demuxer, codec_impl, config)
    }
}

#[cfg(not(feature = "symphonia"))]
mod standalone {
    use super::*;

    pub(super) fn create<B, S>(
        source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        super::super::software::create(source, codec, container, config)
    }
}
