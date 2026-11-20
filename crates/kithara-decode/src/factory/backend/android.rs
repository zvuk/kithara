use kithara_android::media::sys::{KEY_ENCODER_DELAY, KEY_ENCODER_PADDING};
use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{AudioCodec, ContainerFormat};

use super::super::{
    build::finish,
    mpeg::build_mpeg_decoder,
    segment::{build_fmp4_segment_decoder, should_use_segment_aware},
};
use crate::{
    DecodeError, DecodeResult, Decoder, DecoderConfig,
    android::{AndroidCodec, AndroidMediaExtractorDemuxer},
    demuxer::Demuxer,
    gapless::probe_mp4_gapless,
    traits::BoxedSource,
};

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
    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
    {
        if AndroidCodec::supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware Android HW codec path"
            );
            return build_fmp4_segment_decoder(source, layout, config, |track| {
                AndroidCodec::open_with_config(track)
            });
        }
        return Err(DecodeError::UnsupportedCodec { codec });
    }

    if codec == AudioCodec::Pcm && container == Some(ContainerFormat::Wav) {
        return super::super::wave::build_wave_decoder(source, config);
    }

    if matches!(
        codec,
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2
    ) && container == Some(ContainerFormat::Adts)
    {
        return super::super::adts::build_adts_decoder(source, config);
    }

    if android_standalone_supports(codec, container) {
        tracing::debug!(
            ?codec,
            ?container,
            "android-standalone: routing via AMediaExtractor"
        );
        return build_android_standalone_decoder(source, codec, container, config);
    }

    Err(DecodeError::UnsupportedCodec { codec })
}

fn android_standalone_supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
    matches!(
        (codec, container),
        (
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio | ContainerFormat::Wav)
        ) | (AudioCodec::Alac, Some(ContainerFormat::Mp4))
            | (
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2,
                Some(ContainerFormat::Mp4 | ContainerFormat::Fmp4)
            )
            | (
                AudioCodec::Flac,
                Some(ContainerFormat::Flac | ContainerFormat::Mp4 | ContainerFormat::Fmp4)
            )
    )
}

fn build_android_standalone_decoder<B, S>(
    mut source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig<B, S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    if codec == AudioCodec::Mp3 {
        return build_mpeg_decoder(source, container, config, |demuxer| {
            AndroidCodec::open_with_config(demuxer.track_info())
        });
    }
    let gapless = if config.gapless
        && matches!(
            container,
            Some(ContainerFormat::Mp4 | ContainerFormat::Fmp4)
        ) {
        probe_mp4_gapless(&mut source, &config.pools)?
    } else {
        None
    };
    let (demuxer, mut format) = AndroidMediaExtractorDemuxer::open(
        source,
        codec,
        config.byte_map.clone(),
        config.byte_len_handle.clone(),
    )?;
    let mut track = demuxer.track_info().clone();
    track.gapless = gapless;
    if gapless.is_some() || !config.gapless {
        format.set_i32(KEY_ENCODER_DELAY, 0);
        format.set_i32(KEY_ENCODER_PADDING, 0);
    }
    let codec_impl = AndroidCodec::open_with_format(&track, &format)?;
    finish(demuxer, codec_impl, config)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_platform::time::Duration;
    use kithara_resampler::NoResamplerBackend;
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_fixtures::{
        assets::{
            signal_profile_mp3_libmp3lame_11025_1ch, signal_profile_mp3_libmp3lame_44100_1ch,
        },
        without_xing_frame,
    };
    use kithara_test_utils::kithara;

    use crate::{
        DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory,
        test_pools::{TestPools, pools},
    };

    #[kithara::test]
    #[case::tagged_44100_gapless(signal_profile_mp3_libmp3lame_44100_1ch().bytes(), false, 576, true)]
    #[case::tagged_44100_disabled(signal_profile_mp3_libmp3lame_44100_1ch().bytes(), false, 576, false)]
    #[case::tagged_11025_gapless(signal_profile_mp3_libmp3lame_11025_1ch().bytes(), false, 576, true)]
    #[case::tagged_11025_disabled(signal_profile_mp3_libmp3lame_11025_1ch().bytes(), false, 576, false)]
    #[case::headerless_gapless(signal_profile_mp3_libmp3lame_44100_1ch().bytes(), true, 0, true)]
    #[case::headerless_disabled(signal_profile_mp3_libmp3lame_44100_1ch().bytes(), true, 0, false)]
    fn native_mpeg_origin_accounts_for_the_physically_removed_head(
        #[case] bytes: &'static [u8],
        #[case] headerless: bool,
        #[case] origin_frames: u64,
        #[case] gapless: bool,
    ) {
        let bytes = if headerless {
            without_xing_frame(bytes)
        } else {
            bytes.to_vec()
        };
        let media = MediaInfo::builder()
            .codec(AudioCodec::Mp3)
            .container(ContainerFormat::MpegAudio)
            .build();
        let config: DecoderConfig<NoResamplerBackend, TestPools> = DecoderConfig::builder()
            .backend(DecoderBackend::Android)
            .gapless(gapless)
            .pools(pools())
            .build();
        let mut decoder =
            DecoderFactory::create_from_media_info(Cursor::new(bytes), &media, config)
                .expect("native MPEG decoder");
        let origin = decoder
            .spec()
            .duration_for(origin_frames)
            .expect("media origin");
        let actual = decoder.media_origin().expect("native media origin");
        assert!(actual.abs_diff(origin) <= Duration::from_nanos(1));
        for position in [None, Some(Duration::from_secs(1)), Some(Duration::ZERO)] {
            if let Some(position) = position {
                decoder.seek(position).expect("native MPEG seek");
            }
            let DecoderChunkOutcome::Chunk(chunk) = decoder.next_chunk().expect("native MPEG PCM")
            else {
                panic!("resident MPEG input produces PCM");
            };
            assert!(!chunk.samples.is_empty());
            assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
            assert_eq!(chunk.meta.media_origin, Some(actual));
            assert_eq!(decoder.media_origin(), Some(actual));
        }
    }
}
