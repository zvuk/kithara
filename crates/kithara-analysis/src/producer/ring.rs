use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, RingCons, RingProd, SampleBuffer};
use kithara_platform::time::Duration;
use kithara_signal::AudioChunkInfo;
use num_traits::cast::ToPrimitive;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use tracing::warn;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Span {
    pub(crate) start: u64,
    frames: usize,
    pub(crate) channels: usize,
    pub(crate) media_origin: Option<Duration>,
    pub(crate) visible_origin: Option<Duration>,
}

pub(crate) fn open_for<S>(
    pools: &PoolRegion<S>,
    rate: NonZeroU32,
) -> Result<(Writer, Reader), PoolError>
where
    S: HasPool<f32>,
{
    const AHEAD_SECONDS: u32 = 4;
    const AHEAD_RANGES: usize = 256;

    let frames = rate.get().saturating_mul(AHEAD_SECONDS).to_usize();
    open(
        pools,
        frames.unwrap_or(usize::MAX).saturating_mul(2),
        AHEAD_RANGES,
    )
}

pub(crate) fn open<S>(
    pools: &PoolRegion<S>,
    frames: usize,
    ranges: usize,
) -> Result<(Writer, Reader), PoolError>
where
    S: HasPool<f32>,
{
    let (samples_tx, samples_rx) = pools.ring::<f32>(frames)?;
    let (spans_tx, spans_rx) = HeapRb::<Span>::new(ranges.max(1)).split();
    Ok((
        Writer {
            samples: samples_tx,
            spans: spans_tx,
        },
        Reader {
            samples: samples_rx,
            spans: spans_rx,
        },
    ))
}

pub(crate) struct Writer {
    samples: RingProd<SampleBuffer>,
    spans: HeapProd<Span>,
}

impl Writer {
    pub(crate) fn is_open(&self) -> bool {
        self.spans.read_is_held()
    }

    pub(crate) fn push(&mut self, info: &AudioChunkInfo, pcm: &[f32]) -> bool {
        let channels = usize::from(info.spec.channels.max(1));
        let frames = pcm.len() / channels;
        let samples = frames.saturating_mul(channels);
        if frames == 0 || self.samples.vacant_len() < samples || self.spans.is_full() {
            return false;
        }
        self.samples.push_slice(&pcm[..samples]);
        self.spans
            .try_push(Span {
                frames,
                channels,
                start: info.frame_offset,
                media_origin: info.media_origin,
                visible_origin: info.visible_origin,
            })
            .is_ok()
    }
}

pub(crate) struct Reader {
    samples: RingCons<SampleBuffer>,
    spans: HeapCons<Span>,
}

impl Reader {
    pub(crate) fn pop(&mut self, out: &mut SampleBuffer) -> Option<Span> {
        let span = self.spans.try_pop()?;
        out.clear();
        let samples = span.frames.saturating_mul(span.channels);
        if out.ensure_len(samples).is_err() {
            warn!(
                frames = span.frames,
                "analysis ingest: no room to drain a range; dropped"
            );
            self.samples.skip(samples);
            return None;
        }
        let read = self.samples.pop_slice(&mut out[..samples]);
        out.truncate(read);
        Some(span)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::time::Duration;
    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_test_fixtures::{
        fixtures::pcm_ramp,
        unit_fixtures::{
            limiter_peak as twos, trim_codec_priming_drops_leading_frames_and_fades_in as ones,
        },
    };
    use kithara_test_utils::{bufpool::pools, kithara};

    use super::open;

    fn info(frame_offset: u64) -> AudioChunkInfo {
        AudioChunkInfo {
            spec: AudioSpec::new(1, NonZeroU32::new(44_100).expect("test rate is non-zero")),
            frame_offset,
            media_origin: Some(Duration::ZERO),
            visible_origin: Some(Duration::ZERO),
            ..Default::default()
        }
    }

    #[kithara::test]
    fn a_range_comes_out_the_way_it_went_in(pcm_ramp: Vec<f32>) {
        let pools = pools();
        let (mut tx, mut rx) = open(&pools, 64, 4).expect("test ring fits the pools");
        let mut out = pools.get::<f32>();

        assert!(tx.push(&info(100), &pcm_ramp[..4]));
        assert!(tx.push(&info(200), &pcm_ramp[4..6]));

        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(100));
        assert_eq!(&out[..], &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(200));
        assert_eq!(&out[..], &[5.0, 6.0]);
        assert_eq!(
            rx.pop(&mut out).map(|span| span.start),
            None,
            "the ring is drained"
        );
    }

    #[kithara::test]
    fn a_full_ring_refuses_whole_ranges(ones: Vec<f32>, pcm_ramp: Vec<f32>) {
        let pools = pools();
        let (mut tx, mut rx) = open(&pools, 8, 4).expect("test ring fits the pools");
        let mut out = pools.get::<f32>();

        assert!(tx.push(&info(0), &ones[..8]));
        assert!(
            !tx.push(&info(8), &pcm_ramp[1..2]),
            "no room left for a single frame"
        );

        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(0));
        assert_eq!(out.len(), 8, "the refused range left the first one intact");
        assert_eq!(
            rx.pop(&mut out).map(|span| span.start),
            None,
            "the refusal wrote nothing"
        );
    }

    #[kithara::test]
    fn a_full_descriptor_ring_refuses_even_with_room_for_samples(pcm_ramp: Vec<f32>) {
        let pools = pools();
        let (mut tx, mut rx) = open(&pools, 64, 2).expect("test ring fits the pools");
        let mut out = pools.get::<f32>();

        assert!(tx.push(&info(0), &pcm_ramp[0..1]));
        assert!(tx.push(&info(1), &pcm_ramp[1..2]));
        assert!(
            !tx.push(&info(2), &pcm_ramp[2..3]),
            "there are only two range slots"
        );

        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(0));
        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(1));
        assert_eq!(rx.pop(&mut out).map(|span| span.start), None);
    }

    #[kithara::test]
    fn draining_frees_the_room_it_read(ones: Vec<f32>, twos: Vec<f32>) {
        let pools = pools();
        let (mut tx, mut rx) = open(&pools, 8, 4).expect("test ring fits the pools");
        let mut out = pools.get::<f32>();

        assert!(tx.push(&info(0), &ones[..8]));
        assert!(!tx.push(&info(8), &twos[..4]));
        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(0));
        assert!(
            tx.push(&info(8), &twos[..4]),
            "a drained ring takes the range it refused"
        );

        assert_eq!(rx.pop(&mut out).map(|span| span.start), Some(8));
        assert_eq!(&out[..], &[2.0, 2.0, 2.0, 2.0]);
    }
    #[kithara::test]
    fn every_ingest_span_preserves_original_clock_facts(pcm_ramp: Vec<f32>) {
        let pools = pools();
        let (mut tx, mut rx) = open(&pools, 64, 4).expect("test ring fits the pools");
        let mut out = pools.get::<f32>();
        for media_origin in [None, Some(Duration::from_nanos(25_056_689))] {
            let info = AudioChunkInfo {
                media_origin,
                visible_origin: Some(Duration::from_nanos(11_995_465)),
                ..info(22_050)
            };
            assert!(tx.push(&info, &pcm_ramp[..4]));
            let span = rx.pop(&mut out).expect("the accepted span is present");
            assert_eq!(span.start, 22_050);
            assert_eq!(span.media_origin, media_origin);
            assert_eq!(span.visible_origin, info.visible_origin);
            assert_eq!(&out[..], &pcm_ramp[..4]);
        }
    }
}
