use std::{num::NonZeroU16, sync::atomic::Ordering};

use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;
use kithara_stream::ReaderControl;
use symphonia_core::{formats::FormatReader, io::MediaSourceStream};

use crate::{
    DecodeResult, DecoderConfig,
    symphonia::{SymphoniaDemuxer, adapter::ReadSeekAdapter},
    traits::BoxedSource,
};

pub(super) fn open_reader<B, S>(
    source: BoxedSource,
    config: &mut DecoderConfig<B, S>,
    block_align: Option<NonZeroU16>,
    open: impl FnOnce(MediaSourceStream<'static>) -> DecodeResult<Box<dyn FormatReader>>,
) -> DecodeResult<SymphoniaDemuxer>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let control = config
        .reader_control
        .clone()
        .unwrap_or_else(|| ReaderControl::new(None));
    control.require_blocking();
    let adapter = ReadSeekAdapter::new(source, config.byte_len_handle.take(), false)
        .with_control(control.clone());
    let byte_len = adapter.byte_len_handle();
    let byte_pos = adapter.byte_pos_handle();
    let seek_enabled = adapter.seek_enabled_handle();
    let reader = open(MediaSourceStream::from(adapter))?;
    seek_enabled.store(true, Ordering::Release);
    let demuxer = SymphoniaDemuxer::from_reader_with_layout(
        reader,
        Some(byte_pos),
        config.byte_map.take(),
        block_align,
        control,
    )?;
    config.byte_len_handle = Some(byte_len);
    Ok(demuxer)
}
