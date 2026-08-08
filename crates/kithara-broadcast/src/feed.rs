use kithara_platform::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use ringbuf::{
    HeapCons,
    traits::{Consumer, Observer},
};

/// What one [`LivePcmFeed::poll`] saw: the gap it found alongside the samples
/// it appended, and whether the producer is gone.
#[derive(Debug, Clone, Copy, Default)]
pub struct FeedChunk {
    /// Samples the producer dropped since the previous poll. A feed that knows
    /// where its gap fell reports it before the samples that follow it; one
    /// counting alone locates it no closer than the poll.
    pub dropped: u64,
    /// The producer will send nothing further.
    pub has_ended: bool,
}

/// The broadcast's PCM intake: interleaved f32 read without blocking.
///
/// One poll is one consistent view of the feed — the gap, the samples, and
/// end-of-stream — so the worker cannot see the producer leave while samples
/// are still pending.
pub trait LivePcmFeed: Send {
    /// Append whatever interleaved audio is ready onto `out` and report the
    /// gap that came with it. Returns without waiting when nothing is ready.
    fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk;
}

/// Intake over the SPSC ring a real-time producer writes the mix into, paired
/// with the monotonic counter of samples that producer found no room for.
///
/// The ring and the counter are the whole vocabulary between the two sides, so
/// whoever fills the ring stays unknown to the broadcast.
pub struct RingFeed {
    pcm: HeapCons<f32>,
    drops: Arc<AtomicU64>,
    counted: u64,
}

impl RingFeed {
    /// Take the consumer half of the mix ring and the producer's drop counter.
    #[must_use]
    pub fn new(pcm: HeapCons<f32>, drops: Arc<AtomicU64>) -> Self {
        Self {
            pcm,
            drops,
            counted: 0,
        }
    }
}

impl LivePcmFeed for RingFeed {
    /// The producer is read as held before the ring is drained, so one already
    /// gone by then can push nothing more and an empty drain is the end of the
    /// feed.
    fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk {
        let total = self.drops.load(Ordering::Relaxed);
        let dropped = total.saturating_sub(self.counted);
        self.counted = total;

        let held = self.pcm.write_is_held();
        let pending = out.len();
        out.extend(self.pcm.pop_iter());

        FeedChunk {
            dropped,
            has_ended: !held && out.len() == pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use ringbuf::{
        HeapProd, HeapRb,
        traits::{Producer, Split},
    };

    use super::{Arc, AtomicU64, LivePcmFeed, Ordering, RingFeed};

    const CAPACITY: usize = 16;

    fn ring() -> (HeapProd<f32>, Arc<AtomicU64>, RingFeed) {
        let (producer, consumer) = HeapRb::<f32>::new(CAPACITY).split();
        let drops = Arc::new(AtomicU64::new(0));
        let feed = RingFeed::new(consumer, Arc::clone(&drops));
        (producer, drops, feed)
    }

    #[test]
    fn a_gap_is_reported_once() {
        let (mut producer, drops, mut feed) = ring();
        let mut out = Vec::new();

        drops.store(12, Ordering::Relaxed);
        producer.try_push(1.0).expect("room for one sample");
        assert_eq!(feed.poll(&mut out).dropped, 12);

        out.clear();
        assert_eq!(
            feed.poll(&mut out).dropped,
            0,
            "a monotonic counter must not re-report the debt it already reported"
        );

        drops.store(20, Ordering::Relaxed);
        assert_eq!(feed.poll(&mut out).dropped, 8, "only the new gap");
    }

    #[test]
    fn a_released_producer_hands_over_its_remainder_before_the_end() {
        let (mut producer, _drops, mut feed) = ring();
        let mut out = Vec::new();

        producer.push_iter([0.25, -0.25].into_iter());
        drop(producer);

        let chunk = feed.poll(&mut out);
        assert_eq!(out, [0.25, -0.25], "the tail in the ring is handed over");
        assert!(
            !chunk.has_ended,
            "samples still coming out are not the end of the feed"
        );

        out.clear();
        assert!(
            feed.poll(&mut out).has_ended,
            "the feed ends once the released producer's remainder is drained"
        );
        assert!(out.is_empty());
    }

    #[test]
    fn an_empty_ring_with_a_live_producer_polls_empty() {
        let (_producer, _drops, mut feed) = ring();
        let mut out = Vec::new();

        let chunk = feed.poll(&mut out);

        assert!(out.is_empty());
        assert!(!chunk.has_ended, "a live producer keeps the feed open");
        assert_eq!(chunk.dropped, 0);
    }
}
