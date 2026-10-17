use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{AudioCodec, ByteMap, ContainerFormat};

#[cfg(feature = "ape")]
use super::build::create_ape;
use super::{
    build::finish,
    mpeg::build_mpeg_decoder,
    segment::{build_fmp4_segment_decoder, should_use_segment_aware},
};
use crate::{DecodeError, DecodeResult, Decoder, DecoderConfig, traits::BoxedSource};

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
    #[cfg(feature = "ape")]
    if codec == AudioCodec::Ape {
        return create_ape(source, config);
    }
    if codec == AudioCodec::Mp3 && container == Some(ContainerFormat::Wav) {
        let gapless = config.gapless;
        let symphonia_config = crate::symphonia::SymphoniaConfig::builder()
            .gapless(gapless)
            .build();
        return build_mpeg_decoder(source, container, config, |demuxer| {
            use crate::{demuxer::Demuxer, symphonia::SymphoniaCodec};
            SymphoniaCodec::open_with_config(demuxer.track_info(), &symphonia_config)
        });
    }
    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
    {
        return create_segment(source, codec, layout, config);
    }
    create_file_symphonia_universal(source, codec, container, config)
}

fn create_file_symphonia_universal<B, S>(
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
        symphonia::{FileOpen, SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer},
    };

    tracing::debug!(
        ?codec,
        ?container,
        "file-symphonia: dispatching to ComposedDecoder<SymphoniaDemuxer, SymphoniaCodec>"
    );

    let probed_gapless = if config.gapless && container != Some(ContainerFormat::Mp4) {
        scoped_probe(&mut *source, codec, &config.pools)?
    } else {
        None
    };
    let presentations = if container == Some(ContainerFormat::Mp4) {
        crate::mp4::probe_presentations(&mut *source, &config.pools)?
    } else {
        Vec::new()
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
    demuxer.set_mp4_presentations(presentations, config.gapless);
    let symphonia_config = SymphoniaConfig::builder().gapless(config.gapless).build();
    let codec_impl = if SymphoniaCodec::supports(codec) {
        SymphoniaCodec::open_with_config(demuxer.track_info(), &symphonia_config)?
    } else {
        SymphoniaCodec::open_native(&demuxer.native_params)?
    };
    finish(demuxer, codec_impl, config)
}

pub(super) fn create_segment<B, S>(
    source: BoxedSource,
    codec: AudioCodec,
    layout: Arc<dyn ByteMap>,
    config: DecoderConfig<B, S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    use crate::symphonia::{SymphoniaCodec, SymphoniaConfig};

    tracing::debug!(
        ?codec,
        "fmp4_segment: dispatching to segment-aware Symphonia path"
    );
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 | AudioCodec::Flac => {
            let symphonia_config = SymphoniaConfig::builder().gapless(config.gapless).build();
            build_fmp4_segment_decoder(source, layout, config, |track| {
                SymphoniaCodec::open_with_config(track, &symphonia_config)
            })
        }
        other => Err(DecodeError::UnsupportedCodec { codec: other }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_test_fixtures::{assets::signal_mp3_track_sine440_187s, without_xing_frame};
    use kithara_test_utils::kithara;

    use crate::{
        DecoderBackend, DecoderConfig, DecoderFactory,
        test_pools::{TestPools, pools},
    };

    #[kithara::test]
    fn software_factory_estimates_headerless_mp3_duration() {
        let bytes = without_xing_frame(signal_mp3_track_sine440_187s().bytes());
        let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
            DecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .pools(pools())
                .build();
        let decoder = DecoderFactory::create_with_probe(Cursor::new(bytes), Some("mp3"), config)
            .expect("headerless MP3 opens through the software factory");
        let crate::DecoderDuration::RawLength(duration) = decoder
            .duration()
            .expect("headerless MP3 needs bitrate estimation during reader construction")
        else {
            panic!("standalone MP3 duration must describe a raw span");
        };
        assert!((180.0..190.0).contains(&duration.as_secs_f64()));
    }
}
