use std::{
    io::Error as IoError,
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::executor::block_on;
use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, AudioCodec, ContainerFormat, MediaInfo, PlayheadRead, PlayheadState, PlayheadWrite,
    ReadOutcome, SeekControl, SeekObserve, SeekState, Source, SourceError, SourcePhase, Stream,
    StreamResult, StreamType,
};

use crate::{
    signal_pcm::{SignalPcm, signal},
    wav::WavHeader,
};

/// WAV-backed `Source` adapter over [`SignalPcm`].
pub struct SignalSource<S: signal::SignalFn> {
    pcm: SignalPcm<S>,
    seek: Arc<SeekState>,
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
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
            seek: Arc::new(SeekState::new()),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            header: create_header_from_signal(&pcm),
            pcm,
        }
    }

    fn is_past_eof(&self, offset: u64) -> bool {
        self.total_byte_len().is_some_and(|total| offset >= total)
    }

    fn total_byte_len(&self) -> Option<u64> {
        let header_len = self.header.size();

        self.pcm
            .total_pcm_byte_len()
            .map(|data_len| (data_len + header_len) as u64)
    }
}

/// Error type for [`SignalSource`] (currently infallible).
#[derive(Debug, thiserror::Error)]
#[error("signal source error")]
pub struct SignalSourceError;

impl<S: signal::SignalFn + Sync> Source for SignalSource<S> {
    fn len(&self) -> Option<u64> {
        self.total_byte_len()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        Some(
            MediaInfo::builder()
                .channels(self.pcm.channels())
                .codec(AudioCodec::Pcm)
                .container(ContainerFormat::Wav)
                .sample_rate(self.pcm.sample_rate())
                .build(),
        )
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.is_past_eof(range.start) {
            SourcePhase::Eof
        } else {
            SourcePhase::Ready
        }
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        if buf.is_empty() || self.is_past_eof(offset) {
            return Ok(ReadOutcome::Eof);
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

        let Some(count) = NonZeroUsize::new(written) else {
            return Ok(ReadOutcome::Eof);
        };
        Ok(ReadOutcome::Bytes(count))
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn seek_control(&self) -> Arc<dyn SeekControl> {
        Arc::clone(&self.seek) as Arc<dyn SeekControl>
    }

    fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        if self.is_past_eof(range.start) {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }
}

/// `StreamType` marker for [`SignalSource`].
pub struct SignalStream<S: signal::SignalFn>(std::marker::PhantomData<S>);

impl<S: signal::SignalFn + Sync> StreamType for SignalStream<S> {
    type Config = SignalStreamConfig<S>;
    type Events = ();

    type Source = SignalSource<S>;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        config
            .source
            .ok_or_else(|| SourceError::other(IoError::other("no source")))
    }
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
pub fn signal_stream<S: signal::SignalFn + Sync>(
    source: SignalSource<S>,
) -> Stream<SignalStream<S>> {
    let config = SignalStreamConfig {
        source: Some(source),
    };

    block_on(Stream::new(config)).unwrap()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::signal_pcm::{Finite, Infinite};

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
        assert_eq!(src.read_at(total, &mut buf).unwrap(), ReadOutcome::Eof);
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
        assert_eq!(result, ReadOutcome::Bytes(NonZeroUsize::new(8).unwrap()));
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
