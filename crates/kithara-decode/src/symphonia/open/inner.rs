use std::{
    io::{Read, Seek},
    sync::atomic::AtomicU64,
};

use kithara_platform::sync::Arc;
use kithara_stream::{ByteMap, ContainerFormat, ReaderControl};
use symphonia_core::formats::FormatOptions;

use super::{
    super::{SymphoniaConfig, SymphoniaDemuxer},
    probe::{ReaderBootstrap, new_direct, probe_with_seek},
};
use crate::DecodeResult;

/// Inputs to [`SymphoniaDemuxer::open_file`] besides the reader: the
/// format `hint` (file extension), an explicit `container` format that
/// skips probing when known, the bootstrap `byte_len_handle`, and an
/// optional `byte_map` over the underlying source.
pub(crate) struct FileOpen {
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(crate) byte_map: Option<Arc<dyn ByteMap>>,
    pub(crate) container: Option<ContainerFormat>,
    pub(crate) hint: Option<String>,
    pub(crate) control: ReaderControl,
}

impl SymphoniaDemuxer {
    /// Build a demuxer for a file-like source: probe the container if
    /// no [`ContainerFormat`] hint is provided, otherwise wire the
    /// matching reader directly. Returns a [`SymphoniaDemuxer`] plus the
    /// bootstrap byte-length handle (so the factory can keep updating it
    /// across the decoder's lifetime).
    ///
    /// # Errors
    ///
    /// Surfaces probe-side errors verbatim ([`crate::DecodeError::Backend`])
    /// and missing-track errors ([`crate::DecodeError::ProbeFailed`]).
    pub(crate) fn open_file<R>(source: R, open: FileOpen) -> DecodeResult<(Self, Arc<AtomicU64>)>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let FileOpen {
            hint,
            container,
            byte_len_handle,
            byte_map,
            control,
        } = open;
        let config = SymphoniaConfig::builder()
            .maybe_byte_len_handle(byte_len_handle)
            .maybe_hint(hint)
            .reader_control(control.clone())
            .build();
        let format_opts = FormatOptions::default();
        let bootstrap: ReaderBootstrap = if let Some(container) = container {
            new_direct(source, &config, container, format_opts)?
        } else {
            probe_with_seek(source, &config, format_opts, false)?
        };
        let len_handle = bootstrap.byte_len_handle.clone();
        let demuxer = Self::from_reader_with_layout(
            bootstrap.format_reader,
            Some(bootstrap.byte_pos_handle),
            byte_map,
            bootstrap.wav_block_align,
            control,
        )?;
        Ok((demuxer, len_handle))
    }
}
