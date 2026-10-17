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
        mut source: BoxedSource,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        use crate::{
            demuxer::Demuxer,
            gapless::scoped_probe,
            symphonia::{FileOpen, SymphoniaDemuxer},
            webcodecs::codec::WebCodecsCodec,
        };

        if !crate::webcodecs::codec::supports(codec) {
            return super::super::software::create(source, codec, container, config);
        }

        tracing::debug!(
            ?codec,
            ?container,
            "file-symphonia: dispatching to ComposedDecoder<SymphoniaDemuxer, WebCodecsCodec>"
        );
        let probed_gapless = if config.gapless {
            scoped_probe(&mut *source, codec, &config.pools)?
        } else {
            None
        };
        let (mut demuxer, _byte_len) = SymphoniaDemuxer::open_file(
            source,
            FileOpen {
                container,
                hint: config.hint.clone(),
                byte_len_handle: config.byte_len_handle.clone(),
                byte_map: config.byte_map.clone(),
                control: config
                    .reader_control
                    .clone()
                    .unwrap_or_else(|| kithara_stream::ReaderControl::new(None)),
            },
        )?;
        if probed_gapless.is_some() {
            demuxer.set_gapless(probed_gapless);
        }
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
