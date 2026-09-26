#![cfg(target_os = "android")]

use std::{
    io::{self, ErrorKind, Read, Seek, SeekFrom},
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_android::media::{
    AndroidPcmEncoding, MediaDataSource, OutputFormat, OwnedExtractor, OwnedFormat,
    sys::{KEY_CHANNEL_COUNT, KEY_CSD_0, KEY_DURATION_US, KEY_MIME, KEY_SAMPLE_RATE},
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioSpec;

use super::output_spec;
use crate::{
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// Track-level info read out of an extractor track format.
pub(crate) struct TrackFormatInfo {
    pub(crate) mime: String,
    /// `csd-0` blob (AAC `AudioSpecificConfig`, FLAC `STREAMINFO`,
    /// ALAC magic cookie). Empty when the format reports none.
    pub(crate) csd_0: Vec<u8>,
    pub(crate) duration_us: i64,
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
}

/// Streaming length follows the pipeline's publication without seeking the
/// source to EOF. Standalone readers supply their probed fixed length.
enum SourceSize {
    Fixed(Option<u64>),
    Published(Arc<AtomicU64>),
}

/// The pipeline's source as the platform extractor reads it.
struct DataSourceCtx {
    source: BoxedSource,
    error: Option<io::Error>,
    /// Container recognition sees only the separately supplied init segment.
    init_end: Option<u64>,
    size: SourceSize,
}

impl MediaDataSource for DataSourceCtx {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Option<usize> {
        let available = self.init_end.map_or(buf.len(), |end| {
            usize::try_from(end.saturating_sub(offset))
                .map_or(buf.len(), |bytes| bytes.min(buf.len()))
        });
        if available == 0 {
            return Some(0);
        }
        let result = self
            .source
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.source.read(&mut buf[..available]));
        match result {
            Ok(read) => Some(read),
            Err(error) => {
                self.error.get_or_insert(error);
                None
            }
        }
    }

    fn size(&self) -> Option<u64> {
        if let Some(end) = self.init_end {
            return Some(end);
        }
        match &self.size {
            SourceSize::Fixed(size) => *size,
            SourceSize::Published(length) => match length.load(Ordering::Acquire) {
                0 => None,
                length => Some(length),
            },
        }
    }
}

/// PCM output the extractor itself produces, used to keep the sample cursor on
/// the frame grid across a recovery seek.
struct PcmOutput {
    encoding: AndroidPcmEncoding,
    spec: AudioSpec,
}

/// Container parsing through the platform extractor, retaining its selected
/// track format for codec configuration and reporting the extractor's actual
/// seek position.
pub(crate) struct AndroidMediaExtractor {
    pcm_output: Option<PcmOutput>,
    inner: OwnedExtractor<DataSourceCtx>,
    cursor: SampleCursor,
}

#[derive(Clone, Copy, Default)]
enum SampleCursor {
    #[default]
    Current,
    Advance {
        next_pcm: Option<Duration>,
    },
    Recover {
        at: Duration,
    },
}

impl AndroidMediaExtractor {
    pub(crate) fn open(
        mut source: BoxedSource,
        byte_len: Option<Arc<AtomicU64>>,
        init_end: Option<u64>,
    ) -> DecodeResult<Self> {
        let size = match byte_len {
            Some(length) => SourceSize::Published(length),
            None => SourceSize::Fixed(probe_size(&mut source)?),
        };
        let ctx = DataSourceCtx {
            source,
            size,
            init_end,
            error: None,
        };

        let mut inner = OwnedExtractor::open(ctx).map_err(|(mut ctx, error)| {
            ctx.error.take().map_or_else(
                || DecodeError::from(error),
                |source| DecodeError::Io { source },
            )
        })?;
        inner.source_mut().error = None;

        Ok(Self {
            inner,
            cursor: SampleCursor::Current,
            pcm_output: None,
        })
    }

    /// Native seeks floor microsecond timestamps onto the PCM grid, and advancing to fetch the next
    /// sample can block on streaming input.
    fn prepare_sample(&mut self) -> DecodeResult<()> {
        match self.cursor {
            SampleCursor::Current => Ok(()),
            SampleCursor::Recover { at } => {
                let micros = at.as_nanos().div_ceil(1_000);
                let result = self.seek_to(i64::try_from(micros).unwrap_or(i64::MAX));
                if result.is_err() {
                    self.cursor = SampleCursor::Recover { at };
                }
                result.map(|_| ())
            }
            SampleCursor::Advance { next_pcm } => {
                self.cursor = SampleCursor::Current;
                self.inner.advance();
                if let Some(source) = self.inner.source_mut().error.take() {
                    let error = DecodeError::Io { source };
                    if error.pending_reason().is_some()
                        && let Some(at) = next_pcm
                    {
                        self.cursor = SampleCursor::Recover { at };
                    }
                    return Err(error);
                }
                Ok(())
            }
        }
    }

    /// Read the current sample into `buf`. Returns `Ok(Some((n, pts_us)))`
    /// or `Ok(None)` at EOF.
    pub(crate) fn read_sample(&mut self, buf: &mut [u8]) -> DecodeResult<Option<(usize, i64)>> {
        self.prepare_sample()?;
        let Some(read) = self.inner.read_sample(buf) else {
            return self
                .inner
                .source_mut()
                .error
                .take()
                .map_or_else(|| Ok(None), |source| Err(DecodeError::Io { source }));
        };
        let pts_us = self.inner.sample_time_us();
        let next_pcm = self
            .pcm_output
            .as_ref()
            .map(|output| -> DecodeResult<Duration> {
                let bytes_per_sample = match output.encoding {
                    AndroidPcmEncoding::Pcm16 => size_of::<i16>(),
                    AndroidPcmEncoding::Float => size_of::<f32>(),
                };
                let frames = read / (usize::from(output.spec.channels) * bytes_per_sample);
                let pts =
                    Duration::from_micros(u64::try_from(pts_us).map_err(DecodeError::backend)?);
                let next_frame = output
                    .spec
                    .frame_at(pts)
                    .map_err(DecodeError::backend)?
                    .saturating_add(u64::try_from(frames).map_err(DecodeError::backend)?);
                output
                    .spec
                    .duration_for(next_frame)
                    .map_err(DecodeError::backend)
            })
            .transpose()?;
        self.cursor = SampleCursor::Advance { next_pcm };
        Ok(Some((read, pts_us)))
    }

    /// Seek to nearest previous-sync sample at or before `pts_us`.
    pub(crate) fn seek_to(&mut self, pts_us: i64) -> DecodeResult<Option<(Duration, u64)>> {
        /// `AMediaExtractor_getSampleTime` reads -1 as "the extractor holds no sample".
        const NO_SAMPLE: i64 = -1;

        self.inner.source_mut().error = None;
        self.cursor = SampleCursor::Current;
        let seek = self.inner.seek_to_previous_sync(pts_us);
        if let Some(source) = self.inner.source_mut().error.take() {
            return Err(DecodeError::Io { source });
        }
        seek?;
        let landed_us = self.inner.sample_time_us();
        if let Some(source) = self.inner.source_mut().error.take() {
            return Err(DecodeError::Io { source });
        }
        if landed_us == NO_SAMPLE {
            return Ok(None);
        }
        let landed_at =
            Duration::from_micros(u64::try_from(landed_us).map_err(DecodeError::backend)?);
        let landed_byte = self.inner.source_mut().source.stream_position()?;
        Ok(Some((landed_at, landed_byte)))
    }

    /// Track selection can cache EOF at the init boundary.
    pub(crate) fn select_audio_track(&mut self) -> DecodeResult<(TrackFormatInfo, OwnedFormat)> {
        for i in 0..self.inner.track_count() {
            let (info, format) = self.track_info(i)?;
            if info.mime.starts_with("audio/") {
                self.inner.select_track(i)?;
                if info.mime == "audio/raw" {
                    let output = OutputFormat::read(&format)?;
                    self.pcm_output = Some(PcmOutput {
                        encoding: output.pcm_encoding,
                        spec: output_spec(&output)?,
                    });
                }
                if self.inner.source_mut().init_end.take().is_some() {
                    self.cursor = SampleCursor::Recover { at: Duration::ZERO };
                }
                return Ok((info, format));
            }
        }
        Err(DecodeError::InvalidData {
            detail: "no audio track found in extractor",
        })
    }

    fn track_info(&self, idx: usize) -> DecodeResult<(TrackFormatInfo, OwnedFormat)> {
        let format = self.inner.track_format(idx)?;
        let info = read_track_format(&format)?;
        Ok((info, format))
    }
}

fn read_track_format(fmt: &OwnedFormat) -> DecodeResult<TrackFormatInfo> {
    let mime = fmt
        .get_str(KEY_MIME)
        .ok_or(DecodeError::InvalidData {
            detail: "track format missing mime",
        })?
        .to_string_lossy()
        .into_owned();

    let sample_rate = fmt.get_i32(KEY_SAMPLE_RATE).unwrap_or(0);
    let channels = fmt.get_i32(KEY_CHANNEL_COUNT).unwrap_or(0);
    let duration_us = fmt.get_i64(KEY_DURATION_US).unwrap_or(0);
    let csd_0 = fmt.get_buffer(KEY_CSD_0).unwrap_or_default().to_vec();

    let raw_rate = u32::try_from(sample_rate.max(0)).unwrap_or(0);
    if raw_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "android.extractor",
        });
    }
    Ok(TrackFormatInfo {
        mime,
        duration_us,
        csd_0,
        channels: u16::try_from(channels.max(0)).unwrap_or(2),
        sample_rate: raw_rate,
    })
}

/// Total length of `source`, cursor restored to the start.
/// `ErrorKind::Unsupported` is how a source states it has no authoritative
/// length, which the data source expresses as an unknown size.
fn probe_size(source: &mut BoxedSource) -> DecodeResult<Option<u64>> {
    let size = match source.seek(SeekFrom::End(0)) {
        Ok(end) => Some(end),
        Err(err) if err.kind() == ErrorKind::Unsupported => None,
        Err(err) => return Err(DecodeError::backend(err)),
    };
    source
        .seek(SeekFrom::Start(0))
        .map_err(DecodeError::backend)?;
    Ok(size)
}
