use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioObserveError, AudioObserver};
use kithara_dsp::downmix;
use kithara_signal::{AudioChunk, AudioChunkInfo};
use tracing::debug;

use super::ring::Writer;
use crate::analyzer::AnalysisToken;

mod consts {
    /// Frames downmixed per stack block, so an offer never allocates.
    pub(super) const DOWNMIX_BLOCK: usize = 256;
}

/// The producer side of one analysis pass, named once when the handle is made
/// so offering costs no lookup. A track with no open pass has no handle.
pub struct AnalysisProducer {
    token: AnalysisToken,
    rate: NonZeroU32,
    ring: Writer,
}

impl AnalysisProducer {
    pub(crate) const fn new(ring: Writer, rate: NonZeroU32, token: AnalysisToken) -> Self {
        Self { token, rate, ring }
    }

    /// Offer interleaved samples at the metadata's raw source position, retaining
    /// its original clock facts. Downmixes by the channel mean without allocation,
    /// blocking, or retaining `pcm`.
    ///
    /// # Errors
    /// Returns the same bounded-ingest errors as [`AudioObserver::try_observe`].
    pub fn offer(&mut self, pcm: &[f32], info: &AudioChunkInfo) -> Result<(), AudioObserveError> {
        if !self.ring.is_open() {
            return Err(AudioObserveError::Closed);
        }
        let spec = info.spec;
        if spec.sample_rate != self.rate {
            debug!(
                token = self.token.as_str(),
                axis = self.rate.get(),
                rate = spec.sample_rate.get(),
                "analysis ingest: range measured on another axis; refused"
            );
            return Err(AudioObserveError::UnsupportedSampleRate {
                expected: self.rate,
                actual: spec.sample_rate,
            });
        }

        let channels = NonZeroUsize::new(usize::from(spec.channels)).unwrap_or(NonZeroUsize::MIN);
        let frames = pcm.len() / channels;
        let mono = pcm
            .chunks(consts::DOWNMIX_BLOCK.saturating_mul(channels.get()))
            .flat_map(move |block| {
                let mut mono = [0.0_f32; consts::DOWNMIX_BLOCK];
                let written = downmix(block, channels, &mut mono);
                mono.into_iter().take(written)
            });

        self.ring
            .push(info, frames, mono)
            .then_some(())
            .ok_or(AudioObserveError::Full)
    }

    /// The pass this handle feeds.
    #[must_use]
    pub const fn token(&self) -> &AnalysisToken {
        &self.token
    }
}

impl AudioObserver for AnalysisProducer {
    fn try_observe(&mut self, chunk: &AudioChunk) -> Result<(), AudioObserveError> {
        self.offer(&chunk.samples, &chunk.meta)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{AudioObserveError, AudioObserver};
    use kithara_platform::time::Duration;
    use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
    use kithara_test_fixtures::{
        analysis_beat_fixtures::{producer_mono, producer_stereo, producer_unity},
        analysis_fixtures::{analysis_silence, waveform_mix},
    };
    use kithara_test_utils::kithara;

    use super::AnalysisProducer;
    use crate::{
        producer::ring,
        test_pools::{pools, sample_buffer},
    };

    fn spec(rate: u32, channels: u16) -> AudioSpec {
        AudioSpec {
            channels,
            sample_rate: NonZeroU32::new(rate).expect("test rate is non-zero"),
        }
    }

    fn info(rate: u32, channels: u16, frame_offset: u64) -> AudioChunkInfo {
        AudioChunkInfo {
            spec: spec(rate, channels),
            frame_offset,
            media_origin: Some(Duration::ZERO),
            visible_origin: Some(Duration::ZERO),
            ..Default::default()
        }
    }

    fn producer(frames: usize, ranges: usize) -> (AnalysisProducer, ring::Reader) {
        let (tx, rx) = ring::open(&pools(), frames, ranges).expect("test ring fits the pools");
        let rate = NonZeroU32::new(44_100).expect("test rate is non-zero");
        (AnalysisProducer::new(tx, rate, "track-a".into()), rx)
    }

    #[kithara::test]
    fn the_mono_written_is_the_channel_mean(producer_stereo: Vec<f32>) {
        let (mut producer, mut reader) = producer(64, 4);
        let pools = pools();
        let mut out = pools.get::<f32>();

        // Interleaved stereo: L, R, L, R.
        let pcm = producer_stereo;
        assert_eq!(
            producer.offer(&pcm, &info(44_100, 2, 512)),
            Ok(()),
            "the pass axis matches"
        );

        assert_eq!(reader.pop(&mut out).map(|span| span.start), Some(512));
        assert_eq!(
            &out[..],
            &[2.0, -1.0],
            "each frame is the mean of its channels"
        );
    }

    #[kithara::test]
    fn a_foreign_axis_is_refused_without_writing(producer_unity: Vec<f32>) {
        let (mut producer, mut reader) = producer(64, 4);
        let pools = pools();
        let mut out = pools.get::<f32>();

        assert_eq!(
            producer.offer(&producer_unity[..2], &info(48_000, 2, 0)),
            Err(AudioObserveError::UnsupportedSampleRate {
                expected: NonZeroU32::new(44_100).expect("test rate is non-zero"),
                actual: NonZeroU32::new(48_000).expect("test rate is non-zero"),
            })
        );
        assert_eq!(
            reader.pop(&mut out).map(|span| span.start),
            None,
            "nothing reached the transport"
        );
    }

    #[kithara::test]
    fn a_full_transport_reports_the_range_untaken(producer_unity: Vec<f32>) {
        let (mut producer, _reader) = producer(2, 4);

        assert_eq!(
            producer.offer(&producer_unity[..2], &info(44_100, 2, 0)),
            Ok(())
        );
        assert_eq!(
            producer.offer(&producer_unity, &info(44_100, 2, 1)),
            Err(AudioObserveError::Full),
            "a range that does not fit is refused whole"
        );
    }

    #[kithara::test]
    fn an_offer_copies_the_source_buffer(analysis_silence: Vec<f32>) {
        let (mut producer, mut reader) = producer(64, 4);
        let pools = pools();

        let mut chunk = pools.get::<f32>();
        chunk.ensure_len(16).expect("the test pool grows to 16");
        chunk.copy_from_slice(&analysis_silence[..16]);

        assert_eq!(producer.offer(&chunk, &info(44_100, 2, 0)), Ok(()));
        drop(chunk);
        let mut out = pools.get::<f32>();
        assert_eq!(
            reader.pop(&mut out).map(|span| span.start),
            Some(0),
            "the range survives its source buffer going back to the pool"
        );
        assert_eq!(
            out.len(),
            8,
            "two channels of sixteen samples is eight frames"
        );
    }

    #[kithara::test]
    fn a_pass_that_ended_refuses_as_closed(producer_unity: Vec<f32>) {
        let (mut producer, reader) = producer(64, 4);
        assert_eq!(
            producer.offer(&producer_unity[..2], &info(44_100, 2, 0)),
            Ok(())
        );

        drop(reader);
        assert_eq!(
            producer.offer(&producer_unity[..2], &info(44_100, 2, 1)),
            Err(AudioObserveError::Closed),
            "a pass that dropped its half cannot be written to"
        );
    }

    #[kithara::test]
    fn a_mono_source_passes_through_unchanged(producer_mono: Vec<f32>) {
        let (mut producer, mut reader) = producer(64, 4);
        let pools = pools();
        let mut out = pools.get::<f32>();

        let pcm = producer_mono;
        assert_eq!(producer.offer(&pcm, &info(44_100, 1, 0)), Ok(()));

        assert_eq!(reader.pop(&mut out).map(|span| span.start), Some(0));
        assert_eq!(
            &out[..],
            &pcm[..],
            "the mean of one channel is the channel itself"
        );
    }

    #[kithara::test]
    #[case::mono(1)]
    #[case::stereo(2)]
    #[case::six_channels(6)]
    fn a_range_longer_than_one_block_downmixes_every_frame(
        #[case] channels: u16,
        waveform_mix: Vec<f32>,
    ) {
        let frames = 700;
        let width = usize::from(channels);
        let pcm = &waveform_mix[..frames * width];
        let (mut producer, mut reader) = producer(1024, 4);
        let pools = pools();
        let mut out = pools.get::<f32>();

        assert_eq!(producer.offer(pcm, &info(44_100, channels, 0)), Ok(()));
        assert_eq!(reader.pop(&mut out).map(|span| span.start), Some(0));
        let scale = 1.0 / f32::from(channels);
        let want: Vec<u32> = pcm
            .chunks_exact(width)
            .map(|frame| (frame.iter().sum::<f32>() * scale).to_bits())
            .collect();
        let got: Vec<u32> = out.iter().map(|sample| sample.to_bits()).collect();
        assert_eq!(
            got, want,
            "each of {frames} frames is the mean of its channels, bit for bit"
        );
    }
    #[kithara::test]
    fn observing_a_chunk_preserves_both_origins_and_the_raw_offset(producer_mono: Vec<f32>) {
        let (mut producer, mut reader) = producer(64, 4);
        let pools = pools();
        let mut out = pools.get::<f32>();
        let metadata = AudioChunkInfo {
            media_origin: Some(Duration::from_nanos(25_056_689)),
            visible_origin: Some(Duration::from_nanos(11_995_465)),
            frames: u32::try_from(producer_mono.len()).expect("fixture fits u32"),
            ..info(44_100, 1, 22_050)
        };
        let chunk = AudioChunk::new(metadata, sample_buffer(&pools, &producer_mono));
        assert_eq!(producer.try_observe(&chunk), Ok(()));
        let span = reader
            .pop(&mut out)
            .expect("the observed chunk reaches the transport");
        assert_eq!(span.start, metadata.frame_offset);
        assert_eq!(span.media_origin, metadata.media_origin);
        assert_eq!(span.visible_origin, metadata.visible_origin);
        assert_eq!(&out[..], &producer_mono[..]);
    }
}
