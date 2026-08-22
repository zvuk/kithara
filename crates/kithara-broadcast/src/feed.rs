use kithara_platform::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use ringbuf::{
    HeapCons,
    traits::{Consumer, Observer},
};

/// Result of one non-blocking feed poll.
#[derive(Debug, Clone, Copy, Default)]
pub struct FeedChunk {
    /// Samples lost after the audio appended by this poll.
    pub dropped: u64,
    /// The producer will send nothing further.
    pub has_ended: bool,
}

/// Non-blocking interleaved-f32 intake for a live broadcast.
pub trait LivePcmFeed: Send {
    /// Append ready audio and report the gap behind it without waiting.
    fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk;

    /// Bound the feed at already handed-over audio and report its end finitely.
    fn close(&mut self);
}

/// Feed over an SPSC PCM ring and its monotonic dropped-sample counter.
pub struct RingFeed {
    pcm: HeapCons<f32>,
    drops: Arc<AtomicU64>,
    counted: u64,
    last: Option<usize>,
}

impl RingFeed {
    /// Take the PCM consumer and the producer's dropped-sample counter.
    #[must_use]
    pub fn new(pcm: HeapCons<f32>, drops: Arc<AtomicU64>) -> Self {
        Self {
            pcm,
            drops,
            counted: 0,
            last: None,
        }
    }
}

impl LivePcmFeed for RingFeed {
    fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk {
        let held = self.pcm.write_is_held();
        let take = self.last.unwrap_or_else(|| self.pcm.occupied_len());
        let pending = out.len();
        out.resize(pending + take, 0.0);
        let taken = self.pcm.pop_slice(&mut out[pending..]);
        out.truncate(pending + taken);

        let total = self.drops.load(Ordering::Relaxed);
        let dropped = total.saturating_sub(self.counted);
        self.counted = total;

        let has_ended = self.last.as_mut().map_or(!held && taken == 0, |last| {
            *last -= taken;
            *last == 0
        });
        FeedChunk { dropped, has_ended }
    }

    fn close(&mut self) {
        self.last = Some(self.pcm.occupied_len());
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
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

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
    fn a_closed_feed_hands_over_what_the_ring_held_and_ends_there() {
        let (mut producer, _drops, mut feed) = ring();
        let mut out = Vec::new();

        producer.push_iter([0.25, -0.25].into_iter());
        feed.close();
        producer.push_iter([1.0].into_iter());

        let chunk = feed.poll(&mut out);

        assert_eq!(out, [0.25, -0.25], "the ring's audio still goes out");
        assert!(
            chunk.has_ended,
            "a closed feed ends on the audio it held, whatever the producer \
             wrote after"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_empty_ring_with_a_live_producer_polls_empty() {
        let (_producer, _drops, mut feed) = ring();
        let mut out = Vec::new();

        let chunk = feed.poll(&mut out);

        assert!(out.is_empty());
        assert!(!chunk.has_ended, "a live producer keeps the feed open");
        assert_eq!(chunk.dropped, 0);
    }
}
