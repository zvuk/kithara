use std::{
    hint::spin_loop,
    num::{NonZeroU32, NonZeroU64},
};

use kithara_platform::sync::Arc;
use portable_atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use crate::{
    PresentationFrontier, RenderContext, SessionBeat, SessionEpoch, SessionFrame, TransportRevision,
};

const SEQLOCK_PHASES: u64 = 2;

#[derive(Debug, Default)]
struct RenderCell {
    version: AtomicU64,
    output_start: AtomicI64,
    output_end: AtomicI64,
    sample_rate: AtomicU32,
    beat_start: AtomicU64,
    beat_end: AtomicU64,
    beats_present: AtomicU32,
    session_epoch: AtomicU64,
    transport_revision: AtomicU64,
    frontier_source: AtomicU64,
    frontier_output: AtomicI64,
}

impl RenderCell {
    fn clear(&self) {
        self.write(|cell| cell.sample_rate.store(0, Ordering::Relaxed));
    }

    fn load(&self) -> Option<RenderSnapshot> {
        loop {
            let before = self.version.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if !before.is_multiple_of(SEQLOCK_PHASES) {
                spin_loop();
                continue;
            }
            let raw = RawSnapshot {
                output_start: self.output_start.load(Ordering::Relaxed),
                output_end: self.output_end.load(Ordering::Relaxed),
                sample_rate: self.sample_rate.load(Ordering::Relaxed),
                beat_start: self.beat_start.load(Ordering::Relaxed),
                beat_end: self.beat_end.load(Ordering::Relaxed),
                beats_present: self.beats_present.load(Ordering::Relaxed) != 0,
                session_epoch: self.session_epoch.load(Ordering::Relaxed),
                transport_revision: self.transport_revision.load(Ordering::Relaxed),
                frontier_source: self.frontier_source.load(Ordering::Relaxed),
                frontier_output: self.frontier_output.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if self.version.load(Ordering::Acquire) == before {
                return RenderSnapshot::try_from(raw).ok();
            }
            spin_loop();
        }
    }

    fn publish(&self, context: &RenderContext, frontier: PresentationFrontier) {
        self.write(|cell| {
            let output = context.output_frames();
            cell.output_start
                .store(i64::from(output.start), Ordering::Relaxed);
            cell.output_end
                .store(i64::from(output.end), Ordering::Relaxed);
            match context.session_beats() {
                Some(beats) => {
                    cell.beat_start
                        .store(f64::from(beats.start).to_bits(), Ordering::Relaxed);
                    cell.beat_end
                        .store(f64::from(beats.end).to_bits(), Ordering::Relaxed);
                    cell.beats_present.store(1, Ordering::Relaxed);
                }
                None => cell.beats_present.store(0, Ordering::Relaxed),
            }
            cell.session_epoch
                .store(u64::from(context.session_epoch()), Ordering::Relaxed);
            cell.transport_revision.store(
                context.transport_revision().map_or(0, u64::from),
                Ordering::Relaxed,
            );
            cell.frontier_source
                .store(frontier.source(), Ordering::Relaxed);
            cell.frontier_output
                .store(i64::from(frontier.output()), Ordering::Relaxed);
            cell.sample_rate
                .store(context.sample_rate().get(), Ordering::Relaxed);
        });
    }

    fn write(&self, fields: impl FnOnce(&Self)) {
        self.version.fetch_add(1, Ordering::AcqRel);
        fields(self);
        self.version.fetch_add(1, Ordering::Release);
    }
}

struct RawSnapshot {
    output_start: i64,
    output_end: i64,
    sample_rate: u32,
    beat_start: u64,
    beat_end: u64,
    beats_present: bool,
    session_epoch: u64,
    transport_revision: u64,
    frontier_source: u64,
    frontier_output: i64,
}

impl TryFrom<RawSnapshot> for RenderSnapshot {
    type Error = &'static str;

    fn try_from(raw: RawSnapshot) -> Result<Self, Self::Error> {
        let sample_rate = NonZeroU32::new(raw.sample_rate).ok_or("sample rate is zero")?;
        let session_beats = if raw.beats_present {
            Some(
                SessionBeat::new(f64::from_bits(raw.beat_start))
                    .map_err(|_| "beat start is invalid")?
                    ..SessionBeat::new(f64::from_bits(raw.beat_end))
                        .map_err(|_| "beat end is invalid")?,
            )
        } else {
            None
        };
        let transport_revision =
            NonZeroU64::new(raw.transport_revision).map(TransportRevision::from_raw);
        let context = RenderContext::new(
            SessionFrame::new(raw.output_start)..SessionFrame::new(raw.output_end),
            sample_rate,
            session_beats,
            SessionEpoch::new(raw.session_epoch),
            transport_revision,
        )
        .ok_or("render context is invalid")?;
        let frontier = PresentationFrontier::builder()
            .source(raw.frontier_source)
            .output(SessionFrame::new(raw.frontier_output))
            .build();
        Ok(Self { context, frontier })
    }
}

/// Callback-side writer for one resident [`crate::Warp`] render context.
///
/// Publication is allocation-free and lock-free. A Warp has exactly one
/// publisher; clones are handles to that same single-writer cell.
#[derive(Clone, Debug, Default)]
pub struct RenderPublisher(Arc<RenderCell>);

impl RenderPublisher {
    delegate::delegate! {
        to self.0 {
            /// Withdraws the current context at a session-axis discontinuity.
            pub fn clear(&self);
            /// Publishes the exact callback context and its current presentation base.
            pub fn publish(&self, context: &RenderContext, frontier: PresentationFrontier);
        }
    }

    /// Returns the read side paired with this publisher.
    #[must_use]
    pub fn reader(&self) -> RenderReader {
        RenderReader(Arc::clone(&self.0))
    }
}

/// Worker-side reader for one resident [`crate::Warp`] render context.
#[derive(Clone, Debug)]
pub struct RenderReader(Arc<RenderCell>);

impl PartialEq for RenderReader {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RenderReader {}

impl RenderReader {
    #[cfg(feature = "render")]
    pub(crate) fn is_current(&self, snapshot: &RenderSnapshot) -> bool {
        self.load().is_some_and(|current| {
            current.context.session_epoch() == snapshot.context.session_epoch()
        })
    }

    /// Loads one coherent immutable snapshot, or `None` before publication or after clear.
    #[must_use]
    pub fn load(&self) -> Option<RenderSnapshot> {
        self.0.load()
    }
}

/// One immutable callback context paired with its exact presentation base.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct RenderSnapshot {
    context: RenderContext,
    #[field(get, copy)]
    frontier: PresentationFrontier,
}

impl RenderSnapshot {
    #[cfg(feature = "render")]
    pub(crate) fn advance(
        self,
        previous: Option<&Self>,
        source: u64,
        output_frames: usize,
    ) -> Option<Self> {
        let previous = previous
            .filter(|previous| previous.context.session_epoch() == self.context.session_epoch());
        let minimum_source = previous
            .map_or_else(
                || self.frontier.source(),
                |previous| previous.frontier.source(),
            )
            .max(self.frontier.source());
        if source < minimum_source {
            return None;
        }
        let output = previous
            .map_or_else(
                || self.frontier.output(),
                |previous| previous.frontier.output(),
            )
            .max(self.frontier.output());
        let output = i64::from(output).checked_add(i64::try_from(output_frames).ok()?)?;
        let frontier = PresentationFrontier::builder()
            .source(source)
            .output(SessionFrame::new(output))
            .build();
        Some(Self {
            context: self.context,
            frontier,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::RenderPublisher;
    use crate::{
        PresentationFrontier, RenderContext, SessionBeat, SessionEpoch, SessionFrame,
        TransportRevision,
    };

    fn context(epoch: u64, start: i64) -> RenderContext {
        RenderContext::new(
            SessionFrame::new(start)..SessionFrame::new(start + 128),
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            Some(
                SessionBeat::new(1.0).expect("fixture beat is finite")
                    ..SessionBeat::new(1.01).expect("fixture beat is finite"),
            ),
            SessionEpoch::new(epoch),
            Some(TransportRevision::first()),
        )
        .expect("fixture context is valid")
    }

    fn frontier(source: u64, output: i64) -> PresentationFrontier {
        PresentationFrontier::builder()
            .source(source)
            .output(SessionFrame::new(output))
            .build()
    }

    #[kithara::test]
    fn publication_is_one_coherent_snapshot() {
        let publisher = RenderPublisher::default();
        let reader = publisher.reader();
        let expected_context = context(3, 1_000);
        let expected_frontier = frontier(8_000, 1_128);

        publisher.publish(&expected_context, expected_frontier);

        let actual = reader.load().expect("published snapshot is readable");
        assert_eq!(actual.context(), &expected_context);
        assert_eq!(actual.frontier(), expected_frontier);
    }

    #[kithara::test]
    fn clear_withdraws_the_previous_epoch() {
        let publisher = RenderPublisher::default();
        let reader = publisher.reader();
        publisher.publish(&context(3, 1_000), frontier(8_000, 1_128));

        publisher.clear();

        assert!(reader.load().is_none());
    }
}
