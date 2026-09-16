use std::{
    hint::spin_loop,
    num::{NonZeroU32, NonZeroU64},
};

use kithara_platform::sync::Arc;
use kithara_test_macros as kithara;
use portable_atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use crate::{
    PresentationFrontier, RateTarget, RenderContext, SessionBeat, SessionEpoch, SessionFrame,
    SyncMode, TransportRevision, WarpMapRevision,
};

const SEQLOCK_PHASES: u64 = 2;

#[derive(Debug, Default)]
struct RenderCell {
    rate: AtomicU64,
    mode: AtomicU32,
    frontier_output: AtomicI64,
    output_end: AtomicI64,
    output_start: AtomicI64,
    beats_present: AtomicU32,
    sample_rate: AtomicU32,
    beat_end: AtomicU64,
    beat_start: AtomicU64,
    frontier_source: AtomicU64,
    frontier_warp_map: AtomicU64,
    frontier_present: AtomicU32,
    session_epoch: AtomicU64,
    transport_revision: AtomicU64,
    version: AtomicU64,
}

impl RenderCell {
    fn clear(&self) {
        self.write(|cell| {
            cell.frontier_present.store(0, Ordering::Relaxed);
            cell.sample_rate.store(0, Ordering::Relaxed);
        });
    }

    fn load(&self) -> Option<RenderSnapshot> {
        self.load_state().and_then(|state| state.snapshot)
    }

    fn load_state(&self) -> Option<RenderState> {
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
                rate: self.rate.load(Ordering::Relaxed),
                mode: self.mode.load(Ordering::Relaxed),
                output_start: self.output_start.load(Ordering::Relaxed),
                output_end: self.output_end.load(Ordering::Relaxed),
                sample_rate: self.sample_rate.load(Ordering::Relaxed),
                beat_start: self.beat_start.load(Ordering::Relaxed),
                beat_end: self.beat_end.load(Ordering::Relaxed),
                beats_present: self.beats_present.load(Ordering::Relaxed) != 0,
                session_epoch: self.session_epoch.load(Ordering::Relaxed),
                transport_revision: self.transport_revision.load(Ordering::Relaxed),
                frontier_source: self.frontier_source.load(Ordering::Relaxed),
                frontier_warp_map: self.frontier_warp_map.load(Ordering::Relaxed),
                frontier_output: self.frontier_output.load(Ordering::Relaxed),
                frontier_present: self.frontier_present.load(Ordering::Relaxed) != 0,
            };
            fence(Ordering::Acquire);
            if self.version.load(Ordering::Acquire) == before {
                return raw.build_state();
            }
            spin_loop();
        }
    }

    fn publish(&self, context: &RenderContext, frontier: PresentationFrontier) {
        self.write(|cell| {
            cell.rate.store(context.rate().packed(), Ordering::Relaxed);
            cell.mode.store(
                match context.mode() {
                    SyncMode::Off => 0,
                    SyncMode::HostSync => 1,
                    SyncMode::LocalSync => 2,
                },
                Ordering::Relaxed,
            );
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
            cell.frontier_warp_map
                .store(frontier.warp_map().map_or(0, u64::from), Ordering::Relaxed);
            cell.frontier_output
                .store(i64::from(frontier.output()), Ordering::Relaxed);
            cell.frontier_present.store(1, Ordering::Relaxed);
            cell.sample_rate
                .store(context.sample_rate().get(), Ordering::Relaxed);
        });
    }

    fn publish_preparation(&self, context: &RenderContext) {
        self.write(|cell| {
            let same_epoch =
                cell.session_epoch.load(Ordering::Relaxed) == u64::from(context.session_epoch());
            let same_revision = cell.transport_revision.load(Ordering::Relaxed)
                == context.transport_revision().map_or(0, u64::from);
            if !same_epoch || !same_revision {
                cell.frontier_present.store(0, Ordering::Relaxed);
            }
            cell.rate.store(context.rate().packed(), Ordering::Relaxed);
            cell.mode.store(
                match context.mode() {
                    SyncMode::Off => 0,
                    SyncMode::HostSync => 1,
                    SyncMode::LocalSync => 2,
                },
                Ordering::Relaxed,
            );
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
    rate: u64,
    mode: u32,
    beats_present: bool,
    frontier_output: i64,
    output_end: i64,
    output_start: i64,
    sample_rate: u32,
    beat_end: u64,
    beat_start: u64,
    frontier_source: u64,
    frontier_warp_map: u64,
    frontier_present: bool,
    session_epoch: u64,
    transport_revision: u64,
}

impl RawSnapshot {
    fn build_context(&self) -> Option<RenderContext> {
        let sample_rate = NonZeroU32::new(self.sample_rate)?;
        let session_beats = if self.beats_present {
            Some(
                SessionBeat::new(f64::from_bits(self.beat_start)).ok()?
                    ..SessionBeat::new(f64::from_bits(self.beat_end)).ok()?,
            )
        } else {
            None
        };
        let transport_revision =
            NonZeroU64::new(self.transport_revision).map(TransportRevision::from);
        let context = RenderContext::new(
            SessionFrame::new(self.output_start)..SessionFrame::new(self.output_end),
            sample_rate,
            session_beats,
            SessionEpoch::new(self.session_epoch),
            transport_revision,
        )?
        .with_rate(
            match self.mode {
                0 => SyncMode::Off,
                1 => SyncMode::HostSync,
                2 => SyncMode::LocalSync,
                _ => return None,
            },
            RateTarget::unpack(self.rate),
        );
        Some(context)
    }

    fn build_state(self) -> Option<RenderState> {
        let context = self.build_context()?;
        let snapshot = self
            .frontier_present
            .then(|| {
                PresentationFrontier::builder()
                    .source(self.frontier_source)
                    .output(SessionFrame::new(self.frontier_output))
                    .maybe_warp_map(
                        NonZeroU64::new(self.frontier_warp_map).map(WarpMapRevision::from_raw),
                    )
                    .build()
            })
            .map(|frontier| RenderSnapshot {
                frontier,
                context: context.clone(),
            });
        Some(RenderState {
            #[cfg(feature = "render")]
            context,
            snapshot,
        })
    }
}

/// Callback-side writer for one resident [`crate::Warp`] render context.
///
/// Publication is allocation-free and lock-free. A Warp has exactly one
/// publisher; the type is deliberately not cloneable to preserve that invariant.
#[derive(Debug, Default)]
pub struct RenderPublisher(Arc<RenderCell>);

impl RenderPublisher {
    /// Returns the read side paired with this publisher.
    #[must_use]
    pub fn reader(&self) -> RenderReader {
        RenderReader(Arc::clone(&self.0))
    }

    delegate::delegate! {
        to self.0 {
            /// Withdraws the current context at a session-axis discontinuity.
            pub fn clear(&self);
            /// Publishes the exact callback context and its current presentation base.
            #[kithara::probe(
                session_epoch = u64::from(context.session_epoch()),
                transport_revision = context.transport_revision().map_or(0, u64::from),
                output_start = i64::from(context.output_frames().start),
                output_end = i64::from(context.output_frames().end),
                source = frontier.source()
            )]
            pub fn publish(&self, context: &RenderContext, frontier: PresentationFrontier);
            /// Publishes the callback context for preparation without inventing presentation.
            pub fn publish_preparation(&self, context: &RenderContext);
        }
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

    delegate::delegate! {
        to self.0 {
            /// Loads one coherent immutable snapshot, or `None` before publication or after clear.
            #[must_use]
            pub fn load(&self) -> Option<RenderSnapshot>;
            #[cfg(feature = "render")]
            pub(crate) fn load_state(&self) -> Option<RenderState>;
        }
    }
}

/// One immutable callback context paired with its exact presentation base.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct RenderSnapshot {
    #[field(get, copy)]
    frontier: PresentationFrontier,
    context: RenderContext,
}

/// One coherent callback context and optional actual presentation base.
#[derive(Clone, Debug)]
pub(crate) struct RenderState {
    #[cfg(feature = "render")]
    pub(crate) context: RenderContext,
    pub(crate) snapshot: Option<RenderSnapshot>,
}

impl RenderSnapshot {
    /// Rebind this presentation base to an immutable context selected by a plan activation.
    #[cfg(feature = "render")]
    pub(crate) fn with_context(mut self, context: RenderContext) -> Self {
        self.context = context;
        self
    }

    #[cfg(feature = "render")]
    pub(crate) fn preparation_at(
        context: RenderContext,
        source: u64,
        output: SessionFrame,
        warp_map: WarpMapRevision,
    ) -> Self {
        Self {
            frontier: PresentationFrontier::builder()
                .source(source)
                .output(output)
                .warp_map(warp_map)
                .build(),
            context,
        }
    }

    #[cfg(feature = "render")]
    pub(crate) fn prepare_at(
        &self,
        source: u64,
        output: SessionFrame,
        warp_map: WarpMapRevision,
    ) -> Self {
        Self::preparation_at(self.context.clone(), source, output, warp_map)
    }

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
            .maybe_warp_map(self.frontier.warp_map())
            .build();
        Some(Self {
            frontier,
            context: self.context,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::RenderPublisher;
    #[cfg(feature = "render")]
    use super::RenderSnapshot;
    use crate::{
        PresentationFrontier, RateTarget, RenderContext, SessionBeat, SessionEpoch, SessionFrame,
        SyncMode, TransportRevision, WarpMapRevision,
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
        let expected_context = context(3, 1_000)
            .with_rate(SyncMode::LocalSync, RateTarget::default().with_speed(0.75));
        let expected_frontier = PresentationFrontier::builder()
            .source(8_000)
            .output(SessionFrame::new(1_128))
            .warp_map(WarpMapRevision::first())
            .build();

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

    #[kithara::test]
    #[cfg(feature = "render")]
    fn preparation_keeps_the_current_generation_frontier() {
        let publisher = RenderPublisher::default();
        let reader = publisher.reader();
        let presented = frontier(8_000, 1_128);
        publisher.publish(&context(3, 1_000), presented);

        publisher.publish_preparation(&context(3, 1_128));

        let actual = reader
            .load()
            .expect("matching preparation keeps presentation");
        assert_eq!(actual.frontier(), presented);
        assert_eq!(actual.context(), &context(3, 1_128));
    }

    #[kithara::test]
    #[cfg(feature = "render")]
    fn preparation_with_a_new_epoch_withdraws_the_old_frontier() {
        let publisher = RenderPublisher::default();
        let reader = publisher.reader();
        publisher.publish(&context(3, 1_000), frontier(8_000, 1_128));

        let expected = context(4, 2_000);
        publisher.publish_preparation(&expected);

        assert!(reader.load().is_none());
        let state = reader
            .load_state()
            .expect("preparation context is readable");
        assert_eq!(state.context, expected);
        assert!(state.snapshot.is_none());
    }

    #[kithara::test]
    #[cfg(feature = "render")]
    fn preparation_with_a_new_transport_revision_withdraws_the_old_frontier() {
        let publisher = RenderPublisher::default();
        let reader = publisher.reader();
        publisher.publish(&context(3, 1_000), frontier(8_000, 1_128));
        let updated = RenderContext::new(
            SessionFrame::new(2_000)..SessionFrame::new(2_128),
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            None,
            SessionEpoch::new(3),
            Some(TransportRevision::from(
                std::num::NonZeroU64::new(2).expect("fixture revision is non-zero"),
            )),
        )
        .expect("fixture context is valid");

        publisher.publish_preparation(&updated);

        assert!(reader.load().is_none());
        assert!(
            reader
                .load_state()
                .expect("preparation context is readable")
                .snapshot
                .is_none()
        );
    }

    #[kithara::test]
    #[cfg(feature = "render")]
    fn advancing_a_prepared_snapshot_keeps_its_warp_map() {
        let previous_map = WarpMapRevision::first();
        let warp_map = WarpMapRevision::from_raw(
            std::num::NonZeroU64::new(2).expect("fixture revision is non-zero"),
        );
        let previous = RenderSnapshot::preparation_at(
            context(3, 1_000),
            7_900,
            SessionFrame::new(1_000),
            previous_map,
        );
        let advanced = RenderSnapshot::preparation_at(
            context(3, 1_000),
            8_000,
            SessionFrame::new(1_128),
            warp_map,
        )
        .advance(Some(&previous), 8_128, 128)
        .expect("monotonic prepared frontier advances");

        assert_eq!(advanced.frontier().warp_map(), Some(warp_map));
    }
}
