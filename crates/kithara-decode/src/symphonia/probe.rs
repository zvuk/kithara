//! Format-reader construction helpers — direct reader and probe paths.

use std::{
    io::{Read, Seek},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_stream::ContainerFormat;
use symphonia::{
    core::{
        formats::{FormatOptions, FormatReader, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
    },
    default::{
        formats::{AdtsReader, FlacReader, IsoMp4Reader, MpaReader, OggReader, WavReader},
        get_probe,
    },
};

use super::{adapter::ReadSeekAdapter, config::SymphoniaConfig};
use crate::error::{DecodeError, DecodeResult};

pub(crate) struct ReaderBootstrap {
    pub(crate) format_reader: Box<dyn FormatReader>,
    pub(crate) byte_len_handle: Arc<AtomicU64>,
    pub(crate) byte_pos_handle: Arc<AtomicU64>,
}

/// Create a format reader directly from a known container format.
///
/// Seek is disabled during reader construction for streaming-friendly
/// formats (all headers at the start) so readers that otherwise probe
/// the tail of the stream — `IsoMp4Reader` for fMP4, `WavReader` —
/// don't stall an HLS source. Seek is re-enabled unconditionally once
/// the reader is built so subsequent decode/seek operations work.
///
/// Standard MP4 is the exception: its `moov` atom lives at the tail of
/// the file, so the reader must seek to it during construction. This is
/// safe because standard-MP4 consumers pass a fully materialised source.
pub(crate) fn new_direct<R>(
    source: R,
    config: &SymphoniaConfig,
    container: ContainerFormat,
    format_opts: FormatOptions,
) -> DecodeResult<ReaderBootstrap>
where
    R: Read + Seek + Send + Sync + 'static,
{
    let seek_enabled = matches!(container, ContainerFormat::Mp4);
    let adapter = if let Some(ref handle) = config.byte_len_handle {
        ReadSeekAdapter::new_inner(source, Some(Arc::clone(handle)), seek_enabled)
    } else {
        ReadSeekAdapter::new_inner(source, None, seek_enabled)
    };

    let byte_len_handle = adapter.byte_len_handle();
    let seek_enabled_handle = adapter.seek_enabled_handle();
    let byte_pos_handle = adapter.byte_pos_handle();

    let mss = MediaSourceStream::new(Box::new(adapter), MediaSourceStreamOptions::default());

    tracing::debug!(?container, "Creating format reader directly (no probe)");
    let format_reader = create_reader_for_container(mss, container, format_opts)?;

    seek_enabled_handle.store(true, Ordering::Release);
    tracing::debug!("Re-enabled seek after decoder initialization");

    Ok(ReaderBootstrap {
        format_reader,
        byte_len_handle,
        byte_pos_handle,
    })
}

/// Create a format reader via Symphonia's probe mechanism.
///
/// When `seek_enabled` is false, seek is disabled during probe to avoid
/// readers seeking to end to validate file size (e.g. WAV over HLS after
/// an ABR switch). Seek is re-enabled after probe succeeds.
pub(crate) fn probe_with_seek<R>(
    source: R,
    config: &SymphoniaConfig,
    format_opts: FormatOptions,
    seek_enabled: bool,
) -> DecodeResult<ReaderBootstrap>
where
    R: Read + Seek + Send + Sync + 'static,
{
    let adapter = match (&config.byte_len_handle, seek_enabled) {
        (Some(handle), false) => {
            ReadSeekAdapter::new_seek_disabled_shared(source, Arc::clone(handle))
        }
        (Some(handle), true) => ReadSeekAdapter::new_inner(source, Some(Arc::clone(handle)), true),
        (None, true) => ReadSeekAdapter::new_seek_enabled(source),
        (None, false) => ReadSeekAdapter::new_seek_disabled(source),
    };

    let byte_len_handle = adapter.byte_len_handle();
    let seek_enabled_handle = adapter.seek_enabled_handle();
    let byte_pos_handle = adapter.byte_pos_handle();

    let mss = MediaSourceStream::new(Box::new(adapter), MediaSourceStreamOptions::default());

    let mut probe_hint = Hint::new();
    if let Some(ref ext) = config.hint {
        probe_hint.with_extension(ext);
    }

    let meta_opts = MetadataOptions::default();

    tracing::debug!(hint = ?config.hint, seek_enabled, "Probing format");
    let format_reader = get_probe()
        .probe(&probe_hint, mss, format_opts, meta_opts)
        .map_err(|e| DecodeError::Backend(Box::new(e)))?;

    if !seek_enabled {
        seek_enabled_handle.store(true, Ordering::Release);
        tracing::debug!("Re-enabled seek after probe");
    }

    Ok(ReaderBootstrap {
        format_reader,
        byte_len_handle,
        byte_pos_handle,
    })
}

/// Create a format reader directly for a known container format.
fn create_reader_for_container(
    mss: MediaSourceStream<'static>,
    container: ContainerFormat,
    format_opts: FormatOptions,
) -> DecodeResult<Box<dyn FormatReader>> {
    match container {
        ContainerFormat::Mp4 => {
            // Standard MP4 requires probing even if we know the container,
            // because IsoMp4Reader::try_new is optimized for fMP4 and
            // may fail on standard MP4 if moov is not where it expects.
            let mut hint = Hint::new();
            hint.with_extension("mp4");
            let meta_opts = MetadataOptions::default();
            let format_reader = get_probe()
                .probe(&hint, mss, format_opts, meta_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(format_reader)
        }
        ContainerFormat::Fmp4 => {
            let reader = IsoMp4Reader::try_new(mss, format_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(Box::new(reader))
        }
        ContainerFormat::MpegAudio => {
            let reader = MpaReader::try_new(mss, format_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Adts => {
            let reader = AdtsReader::try_new(mss, format_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Flac => {
            let reader = FlacReader::try_new(mss, format_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Wav => {
            let reader = WavReader::try_new(mss, format_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Ogg => {
            let reader = OggReader::try_new(mss, format_opts)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            Ok(Box::new(reader))
        }
        ContainerFormat::MpegTs => Err(DecodeError::UnsupportedContainer(container)),
        ContainerFormat::Caf => {
            tracing::warn!("CAF container - falling back to probe");
            Err(DecodeError::UnsupportedContainer(container))
        }
        ContainerFormat::Mkv => {
            tracing::warn!("MKV container - falling back to probe");
            Err(DecodeError::UnsupportedContainer(container))
        }
    }
}
