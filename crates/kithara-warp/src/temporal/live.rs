use std::{
    hint::spin_loop,
    num::{NonZeroU32, NonZeroU64},
};

use kithara_platform::sync::Arc;
use kithara_signal::{OutputContext, SessionEpoch, SessionFrame, TransportRevision};
use kithara_test_macros as kithara;
use portable_atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use crate::{PresentationFrontier, RenderContext, SessionAnchor, SessionBeat, WarpMapRevision};

const SEQLOCK_PHASES: u64 = 2;

#[derive(Debug, Default)]
struct RenderCell {
    anchor_frame: AtomicI64,
    anchor_beat: AtomicU64,
    anchor_tempo: AtomicU64,
    anchor_target: AtomicU64,
    anchor_smoothing: AtomicU64,
    trajectory_present: AtomicU32,
    frontier_output: AtomicI64,
    output_end: AtomicI64,
    output_start: AtomicI64,
    beats_present: AtomicU32,
    sample_rate: AtomicU32,
    beat_end: AtomicU64,
    beat_start: AtomicU64,
    frontier_source: AtomicU64,
    warp_map: AtomicU64,
    session_epoch: AtomicU64,
    transport_revision: AtomicU64,
    version: AtomicU64,
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
                anchor_frame: self.anchor_frame.load(Ordering::Relaxed),
                anchor_beat: self.anchor_beat.load(Ordering::Relaxed),
                anchor_tempo: self.anchor_tempo.load(Ordering::Relaxed),
                anchor_target: self.anchor_target.load(Ordering::Relaxed),
                anchor_smoothing: self.anchor_smoothing.load(Ordering::Relaxed),
                trajectory_present: self.trajectory_present.load(Ordering::Relaxed) != 0,
                output_start: self.output_start.load(Ordering::Relaxed),
                output_end: self.output_end.load(Ordering::Relaxed),
                sample_rate: self.sample_rate.load(Ordering::Relaxed),
                beat_start: self.beat_start.load(Ordering::Relaxed),
                beat_end: self.beat_end.load(Ordering::Relaxed),
                beats_present: self.beats_present.load(Ordering::Relaxed) != 0,
                session_epoch: self.session_epoch.load(Ordering::Relaxed),
                transport_revision: self.transport_revision.load(Ordering::Relaxed),
                frontier_source: self.frontier_source.load(Ordering::Relaxed),
                warp_map: self.warp_map.load(Ordering::Relaxed),
                frontier_output: self.frontier_output.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if self.version.load(Ordering::Acquire) == before {
                return raw.build();
            }
            spin_loop();
        }
    }

    fn publish(&self, context: &RenderContext, frontier: PresentationFrontier) {
        self.write(|cell| {
            if let Some(anchor) = context.trajectory() {
                cell.anchor_frame
                    .store(i64::from(anchor.frame()), Ordering::Relaxed);
                cell.anchor_beat
                    .store(f64::from(anchor.beat()).to_bits(), Ordering::Relaxed);
                cell.anchor_tempo
                    .store(anchor.beats_per_second().to_bits(), Ordering::Relaxed);
                cell.anchor_target.store(
                    anchor.target_beats_per_second().to_bits(),
                    Ordering::Relaxed,
                );
                cell.anchor_smoothing
                    .store(anchor.smooth_seconds().to_bits(), Ordering::Relaxed);
                cell.trajectory_present.store(1, Ordering::Relaxed);
            } else {
                cell.trajectory_present.store(0, Ordering::Relaxed);
            }
            let output = context.output().output_frames();
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
            cell.session_epoch.store(
                u64::from(context.output().session_epoch()),
                Ordering::Relaxed,
            );
            cell.transport_revision.store(
                context.output().transport_revision().map_or(0, u64::from),
                Ordering::Relaxed,
            );
            cell.warp_map
                .store(frontier.warp_map().map_or(0, u64::from), Ordering::Relaxed);
            cell.frontier_source
                .store(frontier.source(), Ordering::Relaxed);
            cell.frontier_output
                .store(i64::from(frontier.output()), Ordering::Relaxed);
            cell.sample_rate
                .store(context.output().sample_rate().get(), Ordering::Relaxed);
        });
    }

    fn write(&self, fields: impl FnOnce(&Self)) {
        self.version.fetch_add(1, Ordering::AcqRel);
        fields(self);
        self.version.fetch_add(1, Ordering::Release);
    }
}

struct RawSnapshot {
    anchor_frame: i64,
    anchor_beat: u64,
    anchor_tempo: u64,
    anchor_target: u64,
    anchor_smoothing: u64,
    trajectory_present: bool,
    beats_present: bool,
    frontier_output: i64,
    output_end: i64,
    output_start: i64,
    sample_rate: u32,
    beat_end: u64,
    beat_start: u64,
    frontier_source: u64,
    warp_map: u64,
    session_epoch: u64,
    transport_revision: u64,
}

impl RawSnapshot {
    fn build(self) -> Option<RenderSnapshot> {
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
        let output = OutputContext::new(
            SessionFrame::new(self.output_start)..SessionFrame::new(self.output_end),
            sample_rate,
            SessionEpoch::new(self.session_epoch),
            transport_revision,
        )?;
        let context = if self.trajectory_present {
            let frame = SessionFrame::new(self.anchor_frame);
            let anchor = SessionAnchor::new(
                frame,
                SessionBeat::new(f64::from_bits(self.anchor_beat)).ok()?,
                f64::from_bits(self.anchor_tempo),
                sample_rate,
            )
            .ok()?
            .retarget(
                frame,
                f64::from_bits(self.anchor_target),
                f64::from_bits(self.anchor_smoothing),
            )
            .ok()?;
            RenderContext::new(output, Some(anchor))?
        } else {
            RenderContext::new_linear(output, session_beats)?
        };
        let frontier = PresentationFrontier::builder()
            .source(self.frontier_source)
            .maybe_warp_map(NonZeroU64::new(self.warp_map).map(WarpMapRevision::from))
            .output(SessionFrame::new(self.frontier_output))
            .build();
        Some(RenderSnapshot { frontier, context })
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
                session_epoch = u64::from(context.output().session_epoch()),
                transport_revision = context.output().transport_revision().map_or(0, u64::from),
                output_start = i64::from(context.output().output_frames().start),
                output_end = i64::from(context.output().output_frames().end),
                source = frontier.source()
            )]
            pub fn publish(&self, context: &RenderContext, frontier: PresentationFrontier);
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
            current.context.output().session_epoch() == snapshot.context.output().session_epoch()
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
    #[field(get, copy)]
    frontier: PresentationFrontier,
    context: RenderContext,
}

impl RenderSnapshot {
    #[cfg(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee",
        feature = "stretch-glide"
    ))]
    pub(crate) fn bind_output_identity(mut self, revision: Option<WarpMapRevision>) -> Self {
        self.frontier = self.frontier.with_warp_map(revision);
        self
    }

    #[cfg(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee",
        feature = "stretch-glide"
    ))]
    pub(crate) fn mapped(self, cursor: crate::WarpCursor) -> Self {
        Self {
            context: self.context,
            frontier: PresentationFrontier::builder()
                .source(cursor.source())
                .output(cursor.output())
                .warp_map(cursor.revision())
                .build(),
        }
    }

    #[cfg(feature = "render")]
    pub(crate) fn advance(
        self,
        previous: Option<&Self>,
        source: u64,
        output_frames: usize,
    ) -> Option<Self> {
        let previous = previous.filter(|previous| {
            previous.context.output().session_epoch() == self.context.output().session_epoch()
        });
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

    use kithara_signal::{OutputContext, SessionEpoch, SessionFrame, TransportRevision};
    use kithara_test_utils::kithara;

    use super::RenderPublisher;
    use crate::{PresentationFrontier, RenderContext, SessionAnchor, SessionBeat};

    fn context(epoch: u64, start: i64) -> RenderContext {
        let output = OutputContext::new(
            SessionFrame::new(start)..SessionFrame::new(start + 128),
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            SessionEpoch::new(epoch),
            Some(TransportRevision::first()),
        )
        .expect("fixture output range is ordered");
        RenderContext::new_linear(
            output,
            Some(
                SessionBeat::new(1.0).expect("fixture beat is finite")
                    ..SessionBeat::new(1.01).expect("fixture beat is finite"),
            ),
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
        let expected_frontier = PresentationFrontier::builder()
            .source(8_000)
            .output(SessionFrame::new(1_128))
            .warp_map(crate::WarpMapRevision::first())
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
    fn publication_preserves_the_exact_ramp_and_its_subranges() {
        let publisher = RenderPublisher::default();
        let output = context(3, 1_000).output().clone();
        let anchor = SessionAnchor::new(
            SessionFrame::new(900),
            SessionBeat::new(1.0).expect("finite beat"),
            2.0,
            output.sample_rate(),
        )
        .expect("valid anchor")
        .retarget(SessionFrame::new(950), 3.0, 0.005)
        .expect("valid ramp");
        let expected = RenderContext::new(output, Some(anchor)).expect("valid context");
        publisher.publish(&expected, frontier(8_000, 1_128));
        let actual = publisher.reader().load().expect("published snapshot");
        assert_eq!(actual.context(), &expected);
        for split in [1, 17, 64, 127] {
            assert_eq!(
                actual.context().for_output_range(split..128),
                expected.for_output_range(split..128)
            );
        }
    }
    #[kithara::test]
    #[cfg(feature = "render")]
    fn advancing_a_prepared_snapshot_keeps_its_warp_map() {
        let previous_map = crate::WarpMapRevision::first();
        let warp_map = crate::WarpMapRevision::from(
            std::num::NonZeroU64::new(2).expect("fixture revision is non-zero"),
        );
        let previous = super::RenderSnapshot {
            context: context(3, 1_000),
            frontier: frontier(7_900, 1_000).with_warp_map(Some(previous_map)),
        };
        let advanced = super::RenderSnapshot {
            context: context(3, 1_000),
            frontier: frontier(8_000, 1_128).with_warp_map(Some(warp_map)),
        }
        .advance(Some(&previous), 8_128, 128)
        .expect("monotonic prepared frontier advances");

        assert_eq!(advanced.frontier().warp_map(), Some(warp_map));
    }
}
