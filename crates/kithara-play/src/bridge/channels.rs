use std::num::NonZeroUsize;

use firewheel::param::smoother::SmootherConfig;
use kithara_audio::{ScheduledSeek, SeekBegin};
use kithara_events::TrackId;
use kithara_output::LiveOutput;
use kithara_platform::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use kithara_signal::AudioSpec;
use kithara_test_utils::kithara;
use kithara_warp::{
    DEFAULT_RATE_SMOOTHING, RenderReader, RenderSnapshot, SessionFrame, StretchControls,
    WarpMapRevision,
};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};
use smallvec::SmallVec;
use triple_buffer::{Input, Output, triple_buffer};

use super::PlaybackShared;
use crate::{
    bridge::{
        PlayerCmd, PlayerNotification, PreparedLaunchIdentity, ScheduledSeekDisposition, SharedEq,
    },
    rt::{PlayerNodeProcessor, track::PlayerTrack},
    sync::DeckGrid,
};

/// A move of a track's pending synchronized seek onto a successor activation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScheduledSeekReanchor {
    pub(crate) item_id: TrackId,
    pub(crate) position: Duration,
    /// The activation the pending seek must still carry to be moved.
    pub(crate) expected: SessionFrame,
    pub(crate) successor: PreparedLaunchIdentity,
}

/// RT-owned channel halves and playback atomics for one player node.
#[non_exhaustive]
pub struct NodeInputs {
    pub(crate) grid: Output<DeckGrid>,
    pub(crate) stretch: Arc<StretchControls>,
    pub(crate) rate_smoothing: SmootherConfig,
    pub(crate) playback: Arc<PlaybackShared>,
    pub(crate) cmd_rx: HeapCons<PlayerCmd>,
    pub(crate) notif_tx: HeapProd<PlayerNotification>,
    pub(crate) trash_tx: HeapProd<PlayerTrack>,
}

impl NodeInputs {
    /// Supplies the owning player's rate controls before processor construction.
    #[must_use]
    pub fn with_rate(mut self, stretch: Arc<StretchControls>, smoothing: SmootherConfig) -> Self {
        self.stretch = stretch;
        self.rate_smoothing = smoothing;
        self
    }
}

/// Producer for interleaved stereo mix samples and their drop count.
#[non_exhaustive]
pub struct MixTapWriter {
    pub(crate) drops: Arc<AtomicU64>,
    pub(crate) samples: HeapProd<f32>,
}

impl MixTapWriter {
    #[must_use]
    pub fn new(samples: HeapProd<f32>, drops: Arc<AtomicU64>) -> Self {
        Self { drops, samples }
    }
}

impl From<MixTapWriter> for (HeapProd<f32>, Arc<AtomicU64>) {
    fn from(writer: MixTapWriter) -> Self {
        (writer.samples, writer.drops)
    }
}

impl LiveOutput for MixTapWriter {
    fn reconfigure(&mut self, _spec: AudioSpec) {}

    fn write_stereo(&mut self, frames: usize, left: &[f32], right: &[f32]) {
        let stereo = 2;
        let writable = frames
            .min(left.len())
            .min(right.len())
            .min(self.samples.vacant_len() / stereo);
        let pushed = self.samples.push_iter(
            left[..writable]
                .iter()
                .zip(&right[..writable])
                .flat_map(|(&left, &right)| [left, right]),
        );
        let dropped = frames.saturating_mul(stereo).saturating_sub(pushed);
        if dropped > 0 {
            self.drops.fetch_add(
                u64::try_from(dropped).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
    }
}

/// Control-owned channel halves and shared controls for one allocated slot.
#[non_exhaustive]
pub struct SlotControl {
    pub(crate) grid: Input<DeckGrid>,
    pub playback: Arc<PlaybackShared>,
    pub notif_rx: HeapCons<PlayerNotification>,
    pub trash_rx: HeapCons<PlayerTrack>,
    pub cmd_tx: HeapProd<PlayerCmd>,
    pub eq: SharedEq,
    render: RenderBindings,
    seek: SeekBindings,
    scheduled_seeks: SmallVec<[ScheduledTrackSeek; SLOT_TRACKS]>,
    prepared_launch_epochs: SmallVec<[PreparedLaunchHandoff; SLOT_TRACKS]>,
}

/// A prepared launch handed to the callback, with the arm state control last
/// requested for it.
#[derive(Clone, Copy)]
struct PreparedLaunchHandoff {
    item_id: TrackId,
    seek_epoch: u64,
    armed: bool,
}

#[derive(Default)]
struct SeekBindings(Vec<SeekBinding>);

type SeekBinding = (TrackId, Arc<dyn SeekBegin>);

#[derive(Clone, Copy)]
struct ScheduledTrackSeek {
    item_id: TrackId,
    position: Duration,
    disposition: ScheduledSeekDisposition,
    armed: bool,
    state: ScheduledTrackSeekState,
    observed_output: Option<SessionFrame>,
}

#[derive(Clone, Copy)]
enum ScheduledTrackSeekState {
    AwaitingStart,
    AwaitingCommand { seek_epoch: u64 },
}

#[derive(Default)]
struct RenderBindings(SmallVec<[RenderBinding; SLOT_TRACKS]>);

type RenderBinding = (TrackId, RenderReader);

fn render_snapshot_key(snapshot: &RenderSnapshot) -> (u64, i64) {
    let context = snapshot.context();
    (
        u64::from(context.session_epoch()),
        i64::from(context.output_frames().end),
    )
}

const SLOT_TRACKS: usize = PlayerNodeProcessor::MAX_TRACKS;

mod launch;

impl SlotControl {
    pub(crate) fn has_seek_binding(&self, item_id: TrackId) -> bool {
        self.seek.0.iter().any(|(bound_id, _)| *bound_id == item_id)
    }

    pub(crate) fn can_schedule_track_seek(&self, item_id: TrackId) -> bool {
        self.scheduled_seeks
            .iter()
            .any(|seek| seek.item_id == item_id)
            || self.scheduled_seeks.len() < SLOT_TRACKS
    }

    /// Begin a seek on every track this slot holds, off the audio thread.
    pub fn begin_seek(&self, position: Duration) {
        for (_, handle) in &self.seek.0 {
            handle.begin(position);
        }
    }

    pub(crate) fn begin_track_seek(
        &self,
        item_id: TrackId,
        position: Duration,
        disposition: ScheduledSeekDisposition,
    ) -> Option<ScheduledSeek> {
        self.seek
            .0
            .iter()
            .find(|(bound_id, _)| *bound_id == item_id)
            .map(|(_, handle)| {
                let prepared_launch = disposition.is_prepared_launch();
                let seek = if prepared_launch {
                    handle.begin_prepared(position)
                } else {
                    handle.begin_scheduled(position)
                };
                if prepared_launch {
                    kithara_test_macros::probe_event!(
                        prepared_launch_seek_begun,
                        item_id = item_id.as_u64(),
                        seek_epoch = seek.epoch,
                        target_nanos = u64::try_from(position.as_nanos()).unwrap_or(u64::MAX)
                    );
                }
                seek
            })
    }

    pub(crate) fn schedule_track_seek(
        &mut self,
        item_id: TrackId,
        position: Duration,
        disposition: ScheduledSeekDisposition,
    ) {
        debug_assert!(self.can_schedule_track_seek(item_id));
        self.scheduled_seeks.retain(|seek| seek.item_id != item_id);
        self.scheduled_seeks.push(ScheduledTrackSeek {
            item_id,
            position,
            disposition,
            armed: false,
            state: ScheduledTrackSeekState::AwaitingStart,
            observed_output: None,
        });
    }

    #[kithara::hang_watchdog]
    /// Begins and transfers scheduled track seeks.
    ///
    /// A seek on an audible track waits until its activation is within `lead`
    /// output frames of presentation: beginning it discards the decoder's old
    /// position, so the old stream must keep playing until the new epoch only
    /// has the response budget left to prepare. The presentation advance seen
    /// since the previous call is spent ahead of time, so a call cadence
    /// coarser than `lead` still begins the seek before the window closes. A
    /// track with no presented render snapshot is not audible and begins at
    /// once.
    pub(crate) fn service_scheduled_seeks(&mut self, lead: NonZeroUsize) {
        let lead = i64::try_from(lead.get()).unwrap_or(i64::MAX);
        let mut index = 0;
        while index < self.scheduled_seeks.len() {
            hang_reset!();
            let request = self.scheduled_seeks[index];
            if let ScheduledSeekDisposition::SeekOnly { activation } = request.disposition
                && matches!(request.state, ScheduledTrackSeekState::AwaitingStart)
                && let Some(snapshot) = self.render_snapshot_for(request.item_id, None)
            {
                let presented = snapshot.frontier().output();
                let advance = request.observed_output.map_or(0, |observed| {
                    (i64::from(presented) - i64::from(observed)).max(0)
                });
                self.scheduled_seeks[index].observed_output = Some(presented);
                if i64::from(activation) - i64::from(presented) - advance > lead {
                    index += 1;
                    continue;
                }
            }
            if request.disposition.is_prepared_launch()
                && !request.armed
                && matches!(request.state, ScheduledTrackSeekState::AwaitingStart)
            {
                index += 1;
                continue;
            }
            let seek_epoch = match request.state {
                ScheduledTrackSeekState::AwaitingStart => {
                    let Some(seek) = self.begin_track_seek(
                        request.item_id,
                        request.position,
                        request.disposition,
                    ) else {
                        self.scheduled_seeks.remove(index);
                        continue;
                    };
                    if !matches!(seek.outcome, kithara_audio::SeekOutcome::Landed { .. }) {
                        self.scheduled_seeks.remove(index);
                        continue;
                    }
                    self.scheduled_seeks[index].state = ScheduledTrackSeekState::AwaitingCommand {
                        seek_epoch: seek.epoch,
                    };
                    seek.epoch
                }
                ScheduledTrackSeekState::AwaitingCommand { seek_epoch } => seek_epoch,
            };
            if self
                .cmd_tx
                .try_push(PlayerCmd::ScheduleSeek {
                    item_id: request.item_id,
                    seek_epoch,
                    disposition: request.disposition,
                    armed: request.armed,
                })
                .is_ok()
            {
                self.prepared_launch_epochs
                    .retain(|handoff| handoff.item_id != request.item_id);
                if request.disposition.is_prepared_launch() {
                    self.prepared_launch_epochs.push(PreparedLaunchHandoff {
                        item_id: request.item_id,
                        seek_epoch,
                        armed: request.armed,
                    });
                }
                self.scheduled_seeks.remove(index);
            } else {
                index += 1;
            }
        }
    }

    pub(crate) fn bind_render(&mut self, item_id: TrackId, reader: RenderReader) {
        self.render.0.push((item_id, reader));
        kithara_test_macros::probe_event!(
            slot_render_reader_bound,
            item = item_id.as_u64(),
            binding_index =
                u64::try_from(self.render.0.len().saturating_sub(1)).unwrap_or(u64::MAX)
        );
    }

    pub(crate) fn cancel_prepared_launches(&mut self, item_id: TrackId, resume: bool) -> bool {
        let Some(PreparedLaunchHandoff { seek_epoch, .. }) = self
            .prepared_launch_epochs
            .iter()
            .find(|handoff| handoff.item_id == item_id)
            .copied()
        else {
            self.scheduled_seeks
                .retain(|seek| seek.item_id != item_id || !seek.disposition.is_prepared_launch());
            return true;
        };
        // `SlotControl` is accessed only while the engine holds its slots mutex,
        // which also serializes every producer for this command ring. Reserve the
        // carrier before beginning replacement epoch so a decoder promise never
        // escapes without its RT command.
        if self.cmd_tx.vacant_len() == 0 {
            return false;
        }
        let Some((_, handle)) = self
            .seek
            .0
            .iter()
            .find(|(bound_id, _)| *bound_id == item_id)
        else {
            return false;
        };
        let target_seconds = self.playback.position.load(Ordering::Relaxed).max(0.0);
        let target = Duration::from_secs_f64(target_seconds);
        let transport_seek_epoch = self.playback.next_seek_epoch();
        let replacement = handle.begin_prepared(target);
        let command = PlayerCmd::CancelPreparedLaunch {
            item_id,
            prepared_seek_epoch: seek_epoch,
            replacement_seek_epoch: replacement.epoch,
            transport_seek_epoch,
            target,
            resume,
        };
        if self.cmd_tx.try_push(command).is_err() {
            unreachable!(
                "the slots mutex serializes the only command-ring writer after capacity reservation"
            );
        }
        self.prepared_launch_epochs
            .retain(|handoff| handoff.item_id != item_id || handoff.seek_epoch != seek_epoch);
        self.scheduled_seeks
            .retain(|seek| seek.item_id != item_id || !seek.disposition.is_prepared_launch());
        true
    }

    /// Record the control half of a track's seek path.
    pub fn bind_seek(&mut self, item_id: TrackId, handle: Arc<dyn SeekBegin>) {
        self.seek.0.push((item_id, handle));
    }

    pub(crate) fn latest_render_snapshot(&self) -> Option<RenderSnapshot> {
        self.render
            .0
            .iter()
            .filter_map(|(_, reader)| reader.load())
            .max_by_key(|snapshot| {
                let context = snapshot.context();
                (
                    u64::from(context.session_epoch()),
                    i64::from(context.output_frames().end),
                )
            })
    }

    pub(crate) fn render_snapshot_for(
        &self,
        item_id: TrackId,
        warp_map: Option<WarpMapRevision>,
    ) -> Option<RenderSnapshot> {
        let selected = self
            .render
            .0
            .iter()
            .enumerate()
            .filter_map(|(binding_index, binding)| {
                Self::matching_render_snapshot(binding_index, binding, item_id, warp_map)
            })
            .max_by_key(render_snapshot_key);
        kithara_test_macros::probe_event!(
            targeted_render_snapshot_selected,
            target_item = item_id.as_u64(),
            expected_warp_map = warp_map.map_or(0, u64::from),
            selected = if selected.is_some() { 1_u64 } else { 0 },
            selected_warp_map = selected
                .as_ref()
                .and_then(|snapshot| snapshot.frontier().warp_map())
                .map_or(0, u64::from),
            selected_output = selected
                .as_ref()
                .map_or(0, |snapshot| i64::from(snapshot.frontier().output()))
        );
        selected
    }

    fn matching_render_snapshot(
        binding_index: usize,
        binding: &RenderBinding,
        item_id: TrackId,
        warp_map: Option<WarpMapRevision>,
    ) -> Option<RenderSnapshot> {
        let (bound_item, reader) = binding;
        let snapshot = reader.load();
        let snapshot_map = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.frontier().warp_map());
        let matches_item = *bound_item == item_id;
        let matches_map = warp_map.is_none_or(|expected| snapshot_map == Some(expected));
        let binding_index = u64::try_from(binding_index).unwrap_or(u64::MAX);
        kithara_test_macros::probe_event!(
            targeted_render_snapshot_binding,
            target_item = item_id.as_u64(),
            expected_warp_map = warp_map.map_or(0, u64::from),
            bound_item = bound_item.as_u64(),
            binding_index,
            matches_item = u64::from(matches_item)
        );
        kithara_test_macros::probe_event!(
            targeted_render_snapshot_state,
            snapshot_present = u64::from(snapshot.is_some()),
            snapshot_warp_map = snapshot_map.map_or(0, u64::from),
            snapshot_output = snapshot
                .as_ref()
                .map_or(0, |value| i64::from(value.frontier().output())),
            snapshot_epoch = snapshot
                .as_ref()
                .map_or(0, |value| u64::from(value.context().session_epoch())),
            matches_map = u64::from(matches_map)
        );
        matches_item
            .then_some(snapshot)
            .flatten()
            .filter(|_| matches_map)
    }

    pub(crate) fn unbind_render(&mut self, item_id: TrackId, reader: &RenderReader) {
        self.render
            .0
            .retain(|(bound_id, bound_reader)| *bound_id != item_id || bound_reader != reader);
        kithara_test_macros::probe_event!(slot_render_reader_unbound, item = item_id.as_u64());
    }

    /// Forget the exact resource generation returned by the processor.
    pub fn unbind_seek(&mut self, item_id: TrackId, handle: &Arc<dyn SeekBegin>) {
        self.seek.0.retain(|(bound_id, bound_handle)| {
            *bound_id != item_id || !Arc::ptr_eq(bound_handle, handle)
        });
        self.scheduled_seeks.retain(|seek| seek.item_id != item_id);
        self.prepared_launch_epochs
            .retain(|handoff| handoff.item_id != item_id);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroU64},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use kithara_audio::SeekOutcome;
    use kithara_warp::{
        PresentationFrontier, RenderContext, RenderPublisher, SessionEpoch, SessionFrame,
        WarpMapRevision,
    };
    use ringbuf::traits::Consumer;

    use super::*;
    use crate::bridge::PreparedLaunchIdentity;

    const LEAD: NonZeroUsize = NonZeroUsize::new(448).expect("lead is non-zero");

    struct CountSeek(AtomicUsize);

    fn publish_render(publisher: &RenderPublisher, warp_map: WarpMapRevision, output_end: i64) {
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(output_end),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(output_end))
                .warp_map(warp_map)
                .build(),
        );
    }

    /// Fill the command ring so the next control-side push is refused.
    #[kithara::hang_watchdog]
    fn fill_command_ring(control: &mut SlotControl, command: impl Fn() -> PlayerCmd) {
        while control.cmd_tx.try_push(command()).is_ok() {
            hang_reset!();
        }
    }

    #[kithara::test]
    fn item_targeted_snapshot_ignores_equal_output_other_track_order() {
        let target = TrackId::allocate();
        let other = TrackId::allocate();
        let target_map = WarpMapRevision::first();
        let other_map =
            WarpMapRevision::from_raw(NonZeroU64::new(2).expect("fixture revision is non-zero"));

        for target_first in [true, false] {
            let (_, mut control) = slot_channels(SharedEq::new(0));
            let target_publisher = RenderPublisher::default();
            let other_publisher = RenderPublisher::default();
            if target_first {
                control.bind_render(target, target_publisher.reader());
                control.bind_render(other, other_publisher.reader());
            } else {
                control.bind_render(other, other_publisher.reader());
                control.bind_render(target, target_publisher.reader());
            }
            publish_render(&target_publisher, target_map, 2_000);
            publish_render(&other_publisher, other_map, 2_000);

            assert_eq!(
                control
                    .render_snapshot_for(target, Some(target_map))
                    .map(|snapshot| snapshot.frontier().warp_map()),
                Some(Some(target_map))
            );
        }
    }

    #[kithara::test]
    fn item_targeted_snapshot_selects_expected_map_across_generations() {
        let item = TrackId::allocate();
        let old_map = WarpMapRevision::first();
        let expected_map =
            WarpMapRevision::from_raw(NonZeroU64::new(2).expect("fixture revision is non-zero"));
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let old_publisher = RenderPublisher::default();
        let expected_publisher = RenderPublisher::default();
        control.bind_render(item, old_publisher.reader());
        control.bind_render(item, expected_publisher.reader());
        publish_render(&old_publisher, old_map, 2_000);
        publish_render(&expected_publisher, expected_map, 2_000);

        assert_eq!(
            control
                .render_snapshot_for(item, Some(expected_map))
                .map(|snapshot| snapshot.frontier().warp_map()),
            Some(Some(expected_map))
        );
    }

    impl SeekBegin for CountSeek {
        fn begin(&self, position: Duration) -> SeekOutcome {
            self.0.fetch_add(1, Ordering::Relaxed);
            SeekOutcome::Landed {
                target: position,
                landed_at: position,
            }
        }

        fn begin_prepared(&self, position: Duration) -> ScheduledSeek {
            ScheduledSeek {
                epoch: 7,
                outcome: self.begin(position),
            }
        }

        fn begin_scheduled(&self, position: Duration) -> ScheduledSeek {
            ScheduledSeek {
                epoch: 7,
                outcome: self.begin(position),
            }
        }
    }

    #[kithara::test]
    fn track_seek_begins_only_the_named_binding() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let first = TrackId::allocate();
        let second = TrackId::allocate();
        let first_seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        let second_seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(first, first_seek.clone());
        control.bind_seek(second, second_seek.clone());

        let target = Duration::from_secs(3);
        assert_eq!(
            control.begin_track_seek(
                second,
                target,
                ScheduledSeekDisposition::SeekOnly {
                    activation: SessionFrame::new(0)
                }
            ),
            Some(ScheduledSeek {
                epoch: 7,
                outcome: SeekOutcome::Landed {
                    target,
                    landed_at: target,
                },
            })
        );
        assert_eq!(first_seek.0.load(Ordering::Relaxed), 0);
        assert_eq!(second_seek.0.load(Ordering::Relaxed), 1);
    }

    #[kithara::test]
    fn scheduled_track_seek_begins_only_within_the_response_lead_of_its_activation() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::SeekOnly {
                activation: SessionFrame::new(2_000),
            },
        );

        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 0);
        assert!(inputs.cmd_rx.try_pop().is_none());

        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_552)
                .output(SessionFrame::new(1_552))
                .build(),
        );
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek { item_id, seek_epoch: 7, .. }) if item_id == item
        ));

        let prepared_item = TrackId::allocate();
        let prepared_seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(prepared_item, prepared_seek.clone());
        control.schedule_track_seek(
            prepared_item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(2_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );
        control.service_scheduled_seeks(LEAD);
        assert_eq!(prepared_seek.0.load(Ordering::Relaxed), 0);
        assert!(control.set_prepared_launch_armed(prepared_item, true));
        control.service_scheduled_seeks(LEAD);
        assert_eq!(prepared_seek.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                disposition: ScheduledSeekDisposition::PreparedLaunch(_),
                ..
            }) if item_id == prepared_item
        ));
    }

    #[kithara::test]
    fn scheduled_track_seek_spends_the_observed_presentation_advance_ahead_of_its_window() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        let present = |output: i64| {
            publisher.publish(
                &context,
                PresentationFrontier::builder()
                    .source(u64::try_from(output).expect("fixture output is positive"))
                    .output(SessionFrame::new(output))
                    .build(),
            );
        };
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::SeekOnly {
                activation: SessionFrame::new(3_000),
            },
        );

        present(1_000);
        control.service_scheduled_seeks(LEAD);
        present(1_400);
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 0);

        present(2_200);
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek { item_id, seek_epoch: 7, .. }) if item_id == item
        ));
    }

    #[kithara::test]
    fn unarmed_prepared_launch_does_not_begin_or_transfer() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(2_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 0);
        assert!(inputs.cmd_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn a_pending_launch_moves_until_its_successor_falls_within_the_response_lead() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let first = PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: kithara_warp::WarpMapRevision::first(),
        };
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(first),
        );
        let successor =
            |previous: PreparedLaunchIdentity, activation: i64| PreparedLaunchIdentity {
                activation: SessionFrame::new(activation),
                warp_map: previous.warp_map.checked_next().expect("revision"),
            };
        let reanchor = |expected: PreparedLaunchIdentity, successor| ScheduledSeekReanchor {
            item_id: item,
            position: Duration::from_secs(3),
            expected: expected.activation,
            successor,
        };
        let moved = successor(first, 1_900);

        assert!(!control.reanchor_scheduled_seek(reanchor(moved, successor(moved, 1_800)), LEAD));
        assert!(control.reanchor_scheduled_seek(reanchor(first, moved), LEAD));
        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                armed: true,
                disposition: ScheduledSeekDisposition::PreparedLaunch(identity),
                ..
            }) if item_id == item && identity == moved
        ));

        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        assert!(!control.reanchor_scheduled_seek(reanchor(moved, successor(moved, 1_400)), LEAD));
        let restarted = successor(moved, 1_500);
        assert!(control.reanchor_scheduled_seek(reanchor(moved, restarted), LEAD));
        control.service_scheduled_seeks(LEAD);

        assert_eq!(seek.0.load(Ordering::Relaxed), 2);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                armed: true,
                disposition: ScheduledSeekDisposition::PreparedLaunch(identity),
                ..
            }) if item_id == item && identity == restarted
        ));

        control.disarm_prepared_launches();
        let paused = successor(restarted, 1_600);
        assert!(control.reanchor_scheduled_seek(reanchor(restarted, paused), LEAD));
        control.service_scheduled_seeks(LEAD);

        assert_eq!(seek.0.load(Ordering::Relaxed), 2);
        assert!(inputs.cmd_rx.try_pop().is_none());
        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                armed: true,
                disposition: ScheduledSeekDisposition::PreparedLaunch(identity),
                ..
            }) if item_id == item && identity == paused
        ));
    }

    #[kithara::test]
    fn prepared_launch_begins_and_transfers_while_paused_without_a_render_snapshot() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(2_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );
        assert!(control.set_prepared_launch_armed(item, true));

        control.service_scheduled_seeks(LEAD);

        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek { item_id, armed: true, .. }) if item_id == item
        ));
    }

    #[kithara::test]
    fn scheduled_track_seek_retries_command_admission_without_seeking_twice() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        fill_command_ring(&mut control, || PlayerCmd::SetPaused {
            paused: false,
            item_id: None,
        });
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::SeekOnly {
                activation: SessionFrame::new(1_000),
            },
        );

        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(inputs.cmd_rx.try_pop().is_some());

        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(
            std::iter::from_fn(|| inputs.cmd_rx.try_pop()).any(|command| matches!(
                command,
                PlayerCmd::ScheduleSeek { item_id, seek_epoch: 7, .. } if item_id == item
            ))
        );
    }

    #[kithara::test]
    fn prepared_launch_retries_command_admission_without_seeking_twice() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        fill_command_ring(&mut control, || PlayerCmd::SetPaused {
            paused: false,
            item_id: None,
        });
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(1_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );

        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(inputs.cmd_rx.try_pop().is_some());

        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(
            std::iter::from_fn(|| inputs.cmd_rx.try_pop()).any(|command| matches!(
                command,
                PlayerCmd::ScheduleSeek { item_id, seek_epoch: 7, .. } if item_id == item
            ))
        );
    }

    #[kithara::test]
    fn prepared_launch_play_before_transfer_carries_armed_release_through_admission_retry() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        let disposition = ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: kithara_warp::WarpMapRevision::first(),
        });
        control.schedule_track_seek(item, Duration::from_secs(3), disposition);
        fill_command_ring(&mut control, || PlayerCmd::SetFadeDuration(1.0));

        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(
            std::iter::from_fn(|| inputs.cmd_rx.try_pop())
                .all(|command| matches!(command, PlayerCmd::SetFadeDuration(1.0)))
        );

        control.service_scheduled_seeks(LEAD);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                disposition: observed_disposition,
                armed: true,
                ..
            }) if item_id == item && observed_disposition == disposition
        ));
        assert!(inputs.cmd_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn prepared_launch_pause_before_transfer_carries_disarmed_release() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(item, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        let disposition = ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: kithara_warp::WarpMapRevision::first(),
        });
        control.schedule_track_seek(item, Duration::from_secs(3), disposition);
        fill_command_ring(&mut control, || PlayerCmd::SetFadeDuration(1.0));

        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(control.set_prepared_launch_armed(item, false));
        while inputs.cmd_rx.try_pop().is_some() {}

        control.service_scheduled_seeks(LEAD);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                disposition: observed_disposition,
                armed: false,
                ..
            }) if item_id == item && observed_disposition == disposition
        ));
        assert!(inputs.cmd_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn prepared_launch_pause_before_transfer_disarms_every_launch_in_the_slot() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let first = TrackId::allocate();
        let second = TrackId::allocate();
        let first_seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        let second_seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(first, first_seek.clone());
        control.bind_seek(second, second_seek.clone());
        let publisher = RenderPublisher::default();
        control.bind_render(first, publisher.reader());
        let context = RenderContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_128),
            NonZeroU32::new(48_000).expect("fixture sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_000)
                .output(SessionFrame::new(1_000))
                .build(),
        );
        let disposition = ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: kithara_warp::WarpMapRevision::first(),
        });
        control.schedule_track_seek(first, Duration::from_secs(3), disposition);
        control.schedule_track_seek(second, Duration::from_secs(3), disposition);
        assert!(control.set_prepared_launch_armed(first, true));
        assert!(control.set_prepared_launch_armed(second, true));

        control.disarm_prepared_launches();
        control.service_scheduled_seeks(LEAD);

        assert_eq!(first_seek.0.load(Ordering::Relaxed), 0);
        assert_eq!(second_seek.0.load(Ordering::Relaxed), 0);
        assert!(inputs.cmd_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn cancelling_prepared_launches_removes_them_before_a_later_play_can_arm_them() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(2_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );

        assert!(control.cancel_prepared_launches(item, true));

        assert!(!control.set_prepared_launch_armed(item, true));
        assert!(control.scheduled_seeks.is_empty());
        assert!(inputs.cmd_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn cancelling_a_replaced_transferred_launch_cancels_the_transferred_epoch() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        control.bind_seek(item, Arc::new(CountSeek(AtomicUsize::new(0))));
        let launch = ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: kithara_warp::WarpMapRevision::first(),
        });
        control.schedule_track_seek(item, Duration::from_secs(3), launch);
        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        assert!(inputs.cmd_rx.try_pop().is_some());
        control.schedule_track_seek(item, Duration::from_secs(5), launch);

        assert!(control.cancel_prepared_launches(item, true));

        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::CancelPreparedLaunch { item_id, prepared_seek_epoch: 7, .. })
                if item_id == item
        ));
    }

    #[kithara::test]
    fn cancelling_a_transferred_launch_drops_its_queued_replacement() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        control.bind_seek(item, Arc::new(CountSeek(AtomicUsize::new(0))));
        let launch = ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: kithara_warp::WarpMapRevision::first(),
        });
        control.schedule_track_seek(item, Duration::from_secs(3), launch);
        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(LEAD);
        control.schedule_track_seek(item, Duration::from_secs(5), launch);

        assert!(control.cancel_prepared_launches(item, true));

        assert!(!control.set_prepared_launch_armed(item, true));
    }

    #[kithara::test]
    fn full_cancel_command_queue_drops_an_untransferred_prepared_launch() {
        let (mut inputs, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(2_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );
        assert!(control.set_prepared_launch_armed(item, true));

        fill_command_ring(&mut control, || PlayerCmd::SetPaused {
            paused: false,
            item_id: None,
        });
        assert!(control.cancel_prepared_launches(item, true));
        assert!(!control.set_prepared_launch_armed(item, true));
        assert_eq!(
            std::iter::from_fn(|| inputs.cmd_rx.try_pop())
                .filter(|command| matches!(command, PlayerCmd::CancelPreparedLaunch { .. }))
                .count(),
            0
        );
    }

    #[kithara::test]
    fn unbinding_a_track_discards_its_scheduled_seek() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let item = TrackId::allocate();
        let seek = Arc::new(CountSeek(AtomicUsize::new(0)));
        control.bind_seek(item, seek.clone());
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::SeekOnly {
                activation: SessionFrame::new(0),
            },
        );

        let handle: Arc<dyn SeekBegin> = seek.clone();
        control.unbind_seek(item, &handle);

        assert!(control.scheduled_seeks.is_empty());
        assert_eq!(seek.0.load(Ordering::Relaxed), 0);
    }

    #[kithara::test]
    fn scheduled_seeks_stop_at_the_resident_track_capacity() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let items = (0..=SLOT_TRACKS)
            .map(|_| TrackId::allocate())
            .collect::<Vec<_>>();
        for item in items.iter().take(SLOT_TRACKS) {
            control.schedule_track_seek(
                *item,
                Duration::ZERO,
                ScheduledSeekDisposition::SeekOnly {
                    activation: SessionFrame::new(0),
                },
            );
        }

        assert!(!control.can_schedule_track_seek(items[SLOT_TRACKS]));
        assert!(control.can_schedule_track_seek(items[0]));
        assert_eq!(control.scheduled_seeks.len(), SLOT_TRACKS);
    }
}

#[must_use]
pub fn slot_channels(eq: SharedEq) -> (NodeInputs, SlotControl) {
    const COMMAND_CAPACITY: usize = 32;
    const NOTIFICATION_CAPACITY: usize = 32;
    const TRASH_CAPACITY: usize = 64;

    let (cmd_tx, cmd_rx) = HeapRb::<PlayerCmd>::new(COMMAND_CAPACITY).split();
    let (notif_tx, notif_rx) = HeapRb::<PlayerNotification>::new(NOTIFICATION_CAPACITY).split();
    let (trash_tx, trash_rx) = HeapRb::<PlayerTrack>::new(TRASH_CAPACITY).split();
    let playback = Arc::new(PlaybackShared::default());

    let (grid_tx, grid_rx) = triple_buffer(&DeckGrid::default());
    let inputs = NodeInputs {
        grid: grid_rx,
        stretch: StretchControls::new(1.0),
        rate_smoothing: DEFAULT_RATE_SMOOTHING,
        cmd_rx,
        notif_tx,
        trash_tx,
        playback: Arc::clone(&playback),
    };
    let control = SlotControl {
        grid: grid_tx,
        playback,
        notif_rx,
        trash_rx,
        cmd_tx,
        eq,
        seek: SeekBindings::default(),
        scheduled_seeks: SmallVec::new(),
        prepared_launch_epochs: SmallVec::new(),
        render: RenderBindings::default(),
    };
    (inputs, control)
}
