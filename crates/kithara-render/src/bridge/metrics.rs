use kithara_platform::sync::atomic::{AtomicU64, Ordering};

/// Counts the audio thread keeps instead of logging. Monotonic for the life of the mixer, so a
/// reader samples twice and looks at the delta.
#[derive(Debug, Default)]
pub struct RtMetrics {
    decode_errors: AtomicU64,
    evicted_playing: AtomicU64,
    event_overflows: AtomicU64,
    underruns: AtomicU64,
}

/// One read of every [`RtMetrics`] counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get, copy)]
pub struct RtMetricsSnapshot {
    pub(crate) decode_errors: u64,
    pub(crate) evicted_playing: u64,
    pub(crate) event_overflows: u64,
    pub(crate) underruns: u64,
}

impl RtMetrics {
    pub(crate) fn record_decode_error(&self) {
        self.decode_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_event_overflow(&self) {
        self.event_overflows.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_evicted_playing(&self) {
        self.evicted_playing.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_underrun(&self) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> RtMetricsSnapshot {
        RtMetricsSnapshot {
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            evicted_playing: self.evicted_playing.load(Ordering::Relaxed),
            event_overflows: self.event_overflows.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn snapshot_starts_at_zero() {
        assert_eq!(
            RtMetrics::default().snapshot(),
            RtMetricsSnapshot::default()
        );
    }

    #[kithara::test]
    fn each_counter_is_independent() {
        let metrics = RtMetrics::default();
        metrics.record_decode_error();
        metrics.record_evicted_playing();
        metrics.record_evicted_playing();
        metrics.record_event_overflow();
        metrics.record_event_overflow();
        metrics.record_underrun();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.decode_errors(), 1);
        assert_eq!(snapshot.evicted_playing(), 2);
        assert_eq!(snapshot.event_overflows(), 2);
        assert_eq!(snapshot.underruns(), 1);
    }
}
