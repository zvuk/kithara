use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{AudioCodec, ContainerFormat};

use super::{
    build::finish,
    probe::{skip_id3_tags, wav_data_range},
};
use crate::{DecodeError, DecodeResult, Decoder, DecoderConfig, traits::BoxedSource};

/// MPEG audio through the demuxer that rolls a packet read back when it meets bytes still in
/// flight, so a stalled read resumes at the same packet and a seek reads only from where it lands.
pub(super) fn build_mpeg_decoder<C>(
    mut source: BoxedSource,
    container: Option<ContainerFormat>,
    mut config: DecoderConfig<
        impl ResamplerBackend,
        impl HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    >,
    open_codec: impl FnOnce(&mut crate::symphonia::SymphoniaDemuxer) -> DecodeResult<C>,
) -> DecodeResult<Box<dyn Decoder>>
where
    C: crate::codec::FrameCodec + 'static,
{
    use kithara_mpa::MpaReader;
    use symphonia_core::{
        formats::FormatOptions,
        io::{MediaSourceStream, ReadBytes},
    };

    use crate::{
        gapless::scoped_probe,
        symphonia::{SymphoniaDemuxer, adapter::ReadSeekAdapter},
    };

    let audio_start = skip_id3_tags(&mut source)?;
    let range = if container == Some(ContainerFormat::Wav) {
        Some(wav_data_range(&mut source)?)
    } else {
        None
    };
    source.seek(std::io::SeekFrom::Start(0))?;
    let gapless = if config.gapless {
        scoped_probe(&mut *source, AudioCodec::Mp3, &config.pools)?
    } else {
        None
    };
    let control = config
        .reader_control
        .clone()
        .unwrap_or_else(|| kithara_stream::ReaderControl::new(None));
    control.require_blocking();
    let adapter = ReadSeekAdapter::new(source, config.byte_len_handle.take(), true)
        .with_control(control.clone());
    let adapter = match range {
        Some(range) => adapter.with_range(range)?,
        None => adapter,
    };
    let byte_len_handle = adapter.byte_len_handle();
    let byte_pos_handle = adapter.byte_pos_handle();
    let mut stream = MediaSourceStream::from(adapter);
    if container != Some(ContainerFormat::Wav) {
        stream
            .ignore_bytes(audio_start)
            .map_err(DecodeError::backend)?;
    }
    let reader =
        MpaReader::try_new(stream, FormatOptions::default()).map_err(DecodeError::backend)?;
    let mut demuxer = SymphoniaDemuxer::from_reader_with_layout(
        Box::new(reader),
        Some(byte_pos_handle),
        config.byte_map.take(),
        None,
        control,
    )?;
    demuxer.set_gapless(gapless);
    let codec = open_codec(&mut demuxer)?;
    config.byte_len_handle = Some(byte_len_handle);
    finish(demuxer, codec, config)
}
