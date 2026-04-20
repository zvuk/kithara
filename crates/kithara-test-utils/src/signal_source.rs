//! On-demand signal generator.
//!
//! `SignalPcm<S>` is the PCM-first core that creates interleaved samples.

use std::{io, io::Error as IoError, ops::Range};

use futures::executor::block_on;
use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, ReadOutcome, Source, SourcePhase, Stream, StreamResult,
    StreamType, Timeline,
};

use crate::{
    signal_pcm::{SignalPcm, signal},
    wav::WavHeader,
};

/// WAV-backed `Source` adapter over [`SignalPcm`].
pub struct SignalSource<S: signal::SignalFn> {
    pcm: SignalPcm<S>,
    timeline: Timeline,
    header: WavHeader,
}

fn create_header_from_signal<S: signal::SignalFn>(pcm: &SignalPcm<S>) -> WavHeader {
    WavHeader::new(pcm.sample_rate(), pcm.channels(), pcm.total_pcm_byte_len())
}

impl<S: signal::SignalFn> SignalSource<S> {
    /// Creates a signal source over `SignalPcm`
    #[must_use]
    pub fn new(pcm: SignalPcm<S>) -> Self {
        Self {
            timeline: Timeline::new(),
            header: create_header_from_signal(&pcm),
            pcm,
        }
    }

    fn total_byte_len(&self) -> Option<u64> {
        let header_len = self.header.size();

        self.pcm
            .total_pcm_byte_len()
            .map(|data_len| (data_len + header_len) as u64)
    }

    fn is_past_eof(&self, offset: u64) -> bool {
        self.total_byte_len().is_some_and(|total| offset >= total)
    }
}

/// Error type for [`SignalSource`] (currently infallible).
#[derive(Debug, thiserror::Error)]
#[error("signal source error")]
pub struct SignalSourceError;

impl<S: signal::SignalFn> Source for SignalSource<S> {
    type Error = SignalSourceError;

    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        _timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        if self.is_past_eof(range.start) {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        if buf.is_empty() || self.is_past_eof(offset) {
            return Ok(ReadOutcome::Data(0));
        }

        let mut written = 0usize;
        let mut pos = offset;

        let header_size = self.header.size() as u64;
        let header = self.header.as_ref();

        if pos < header_size {
            let header_remaining = (header_size - pos) as usize;
            let n = header_remaining.min(buf.len());
            buf[..n].copy_from_slice(&header[pos as usize..pos as usize + n]);
            written += n;
            pos += n as u64;
        }

        if written < buf.len() {
            let pcm_offset = pos.saturating_sub(header_size);
            let pcm_max = self.pcm.total_pcm_byte_len().unwrap_or(usize::MAX);
            written += self
                .pcm
                .render_pcm(pcm_offset as usize, pcm_max, &mut buf[written..]);
        }

        Ok(ReadOutcome::Data(written))
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.is_past_eof(range.start) {
            SourcePhase::Eof
        } else {
            SourcePhase::Ready
        }
    }

    fn len(&self) -> Option<u64> {
        self.total_byte_len()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        Some(
            MediaInfo::default()
                .with_channels(self.pcm.channels())
                .with_codec(AudioCodec::Pcm)
                .with_container(ContainerFormat::Wav)
                .with_sample_rate(self.pcm.sample_rate()),
        )
    }
}

/// `StreamType` marker for [`SignalSource`].
pub struct SignalStream<S: signal::SignalFn>(std::marker::PhantomData<S>);

impl<S: signal::SignalFn> StreamType for SignalStream<S> {
    type Config = SignalStreamConfig<S>;
    type Source = SignalSource<S>;
    type Error = io::Error;

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config.source.ok_or_else(|| IoError::other("no source"))
    }

    type Events = ();
}

/// Configuration for [`SignalStream`].
pub struct SignalStreamConfig<S: signal::SignalFn> {
    /// Pre-built source to hand off to the stream.
    pub source: Option<SignalSource<S>>,
}

impl<S: signal::SignalFn> Default for SignalStreamConfig<S> {
    fn default() -> Self {
        Self { source: None }
    }
}

/// Create a `Stream` from a WAV-backed [`SignalSource`].
#[must_use]
pub fn signal_stream<S: signal::SignalFn>(source: SignalSource<S>) -> Stream<SignalStream<S>> {
    let config = SignalStreamConfig {
        source: Some(source),
    };

    block_on(Stream::new(config)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kithara,
        signal_pcm::{Finite, Infinite},
    };

    #[kithara::test]
    fn finite_eof() {
        let sample_rate = 48000;
        let pcm = SignalPcm::new(
            signal::SineWave(440.0),
            sample_rate,
            2,
            Finite::from_duration(Duration::from_millis(1), sample_rate),
        );

        let mut src = SignalSource::new(pcm);
        let total = src.len().unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(src.read_at(total, &mut buf).unwrap(), ReadOutcome::Data(0));
    }

    #[kithara::test]
    fn media_info_correct() {
        let pcm = SignalPcm::new(signal::SineWave(440.0), 48000, 2, Infinite);
        let src = SignalSource::new(pcm);
        let info = src.media_info().unwrap();
        assert_eq!(info.codec, Some(AudioCodec::Pcm));
        assert_eq!(info.container, Some(ContainerFormat::Wav));
        assert_eq!(info.sample_rate, Some(48000));
        assert_eq!(info.channels, Some(2));
    }

    #[kithara::test]
    fn read_spanning_header_and_pcm() {
        let pcm = SignalPcm::new(signal::Silence, 44100, 1, Infinite);
        let mut src = SignalSource::new(pcm);
        let mut buf = [0xFFu8; 8];
        let result = src.read_at(40, &mut buf).unwrap();
        assert_eq!(result, ReadOutcome::Data(8));
        assert_eq!(&buf[0..4], &0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(&buf[4..8], &[0, 0, 0, 0]);
    }

    #[kithara::test]
    fn signal_stream_creates_stream() {
        let pcm = SignalPcm::new(signal::SineWave(440.0), 44100, 2, Infinite);
        let src = SignalSource::new(pcm);
        let stream = signal_stream(src);
        assert_eq!(stream.len(), None);
    }
}
