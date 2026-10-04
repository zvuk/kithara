use std::{
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_mpa::MpaReader;
use kithara_platform::sync::Arc;
use kithara_stream::ContainerFormat;
use symphonia::{
    core::{
        formats::{FormatOptions, FormatReader, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions, ReadBytes},
        meta::MetadataOptions,
    },
    default::formats::{AdtsReader, AiffReader, FlacReader, IsoMp4Reader, OggReader, WavReader},
};

use super::{adapter::ReadSeekAdapter, config::SymphoniaConfig, registry::get_probe};
use crate::error::{DecodeError, DecodeResult};

pub(crate) struct ReaderBootstrap {
    pub(crate) byte_len_handle: Arc<AtomicU64>,
    pub(crate) byte_pos_handle: Arc<AtomicU64>,
    pub(crate) format_reader: Box<dyn FormatReader>,
}

/// Create a format reader directly from a known container format.
///
/// Seek is disabled during reader construction for streaming-friendly
/// formats (all headers at the start) so readers that otherwise probe
/// the tail of the stream — `IsoMp4Reader` for fMP4, `WavReader` —
/// don't stall an HLS source. Seek is re-enabled unconditionally once
/// the reader is built so subsequent decode/seek operations work.
///
/// MP4 and Ogg require seek during construction: Ogg reads the terminal
/// granule position to establish the track duration. MP4's `moov` atom lives at the tail of
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
    let seek_enabled = matches!(container, ContainerFormat::Mp4 | ContainerFormat::Ogg);
    let mut source: crate::traits::BoxedSource = Box::new(source);
    let audio_start = crate::factory::skip_id3_tags(&mut source)?;
    source.seek(SeekFrom::Start(0))?;
    let adapter = ReadSeekAdapter::new(
        source,
        config.byte_len_handle.as_ref().map(Arc::clone),
        seek_enabled,
    );

    let byte_len_handle = adapter.byte_len_handle();
    let seek_enabled_handle = adapter.seek_enabled_handle();
    let byte_pos_handle = adapter.byte_pos_handle();

    let mut mss = MediaSourceStream::new(Box::new(adapter), MediaSourceStreamOptions::default());
    mss.ignore_bytes(audio_start)
        .map_err(DecodeError::backend)?;

    tracing::debug!(?container, "Creating format reader directly (no probe)");
    let format_reader = create_reader_for_container(mss, container, format_opts)?;

    seek_enabled_handle.store(true, Ordering::Release);
    tracing::debug!("Re-enabled seek after decoder initialization");

    Ok(ReaderBootstrap {
        byte_len_handle,
        byte_pos_handle,
        format_reader,
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
    let adapter = ReadSeekAdapter::new(
        source,
        config.byte_len_handle.as_ref().map(Arc::clone),
        seek_enabled,
    );

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
        .map_err(DecodeError::backend)?;

    if !seek_enabled {
        seek_enabled_handle.store(true, Ordering::Release);
        tracing::debug!("Re-enabled seek after probe");
    }

    Ok(ReaderBootstrap {
        byte_len_handle,
        byte_pos_handle,
        format_reader,
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
            let mut hint = Hint::new();
            hint.with_extension("mp4");
            let meta_opts = MetadataOptions::default();
            let format_reader = get_probe()
                .probe(&hint, mss, format_opts, meta_opts)
                .map_err(DecodeError::backend)?;
            Ok(format_reader)
        }
        ContainerFormat::Fmp4 => {
            let reader = IsoMp4Reader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::MpegAudio => {
            let reader = MpaReader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Adts => {
            let reader = AdtsReader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Flac => {
            let reader = FlacReader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Wav => {
            let reader = WavReader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Aiff => {
            let reader = AiffReader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::Ogg => {
            let reader = OggReader::try_new(mss, format_opts).map_err(DecodeError::backend)?;
            Ok(Box::new(reader))
        }
        ContainerFormat::MpegTs | ContainerFormat::Ape => {
            Err(DecodeError::UnsupportedContainer { container })
        }
        ContainerFormat::Caf => {
            tracing::warn!("CAF container - falling back to probe");
            Err(DecodeError::UnsupportedContainer { container })
        }
        ContainerFormat::Mkv => {
            tracing::warn!("MKV container - falling back to probe");
            Err(DecodeError::UnsupportedContainer { container })
        }
    }
}
