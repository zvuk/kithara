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
use kithara_warp::{DEFAULT_RATE_SMOOTHING, RenderReader, RenderSnapshot, StretchControls};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};
use smallvec::SmallVec;
use triple_buffer::{Input, Output, triple_buffer};

use super::PlaybackShared;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, ScheduledSeekDisposition, SharedEq},
    rt::{PlayerNodeProcessor, track::PlayerTrack},
    sync::DeckGrid,
};

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
    scheduled_seeks: Vec<ScheduledTrackSeek>,
    prepared_launch_epochs: Vec<(TrackId, u64)>,
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
}

#[derive(Clone, Copy)]
enum ScheduledTrackSeekState {
    AwaitingStart,
    AwaitingCommand { seek_epoch: u64 },
}

#[derive(Default)]
struct RenderBindings(SmallVec<[RenderBinding; SLOT_TRACKS]>);

type RenderBinding = (TrackId, RenderReader);

const SLOT_TRACKS: usize = PlayerNodeProcessor::MAX_TRACKS;

impl SlotControl {
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
        self.scheduled_seeks.retain(|seek| seek.item_id != item_id);
        self.prepared_launch_epochs
            .retain(|(bound_id, _)| *bound_id != item_id);
        self.scheduled_seeks.push(ScheduledTrackSeek {
            item_id,
            position,
            disposition,
            armed: false,
            state: ScheduledTrackSeekState::AwaitingStart,
        });
    }

    pub(crate) fn service_scheduled_seeks(&mut self, preparation_frames: usize) {
        let preparation_end = self.latest_render_snapshot().map(|snapshot| {
            i64::from(snapshot.frontier().output())
                .saturating_add(i64::try_from(preparation_frames).unwrap_or(i64::MAX))
        });
        let mut index = 0;
        while index < self.scheduled_seeks.len() {
            let request = self.scheduled_seeks[index];
            if request.disposition.is_prepared_launch()
                && !request.armed
                && matches!(request.state, ScheduledTrackSeekState::AwaitingStart)
            {
                index += 1;
                continue;
            }
            if !request.disposition.is_prepared_launch()
                && preparation_end
                    .is_none_or(|end| i64::from(request.disposition.activation()) > end)
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
                if request.disposition.is_prepared_launch() {
                    self.prepared_launch_epochs
                        .retain(|(item_id, _)| *item_id != request.item_id);
                    self.prepared_launch_epochs
                        .push((request.item_id, seek_epoch));
                }
                self.scheduled_seeks.remove(index);
            } else {
                index += 1;
            }
        }
    }

    pub(crate) fn bind_render(&mut self, item_id: TrackId, reader: RenderReader) {
        self.render.0.push((item_id, reader));
    }

    pub(crate) fn set_prepared_launch_armed(&mut self, item_id: TrackId, armed: bool) -> bool {
        let Some(seek) = self
            .scheduled_seeks
            .iter_mut()
            .find(|seek| seek.item_id == item_id)
        else {
            return false;
        };
        if !seek.disposition.is_prepared_launch() {
            return false;
        }
        seek.armed = armed;
        true
    }

    pub(crate) fn disarm_prepared_launches(&mut self) {
        for seek in &mut self.scheduled_seeks {
            if seek.disposition.is_prepared_launch() {
                seek.armed = false;
            }
        }
    }

    pub(crate) fn cancel_prepared_launches(&mut self, item_id: TrackId, resume: bool) -> bool {
        let Some((_, seek_epoch)) = self
            .prepared_launch_epochs
            .iter()
            .find(|(bound_id, _)| *bound_id == item_id)
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
            .retain(|(bound_id, epoch)| *bound_id != item_id || *epoch != seek_epoch);
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

    pub(crate) fn unbind_render(&mut self, item_id: TrackId, reader: &RenderReader) {
        self.render
            .0
            .retain(|(bound_id, bound_reader)| *bound_id != item_id || bound_reader != reader);
    }

    /// Forget the exact resource generation returned by the processor.
    pub fn unbind_seek(&mut self, item_id: TrackId, handle: &Arc<dyn SeekBegin>) {
        self.seek.0.retain(|(bound_id, bound_handle)| {
            *bound_id != item_id || !Arc::ptr_eq(bound_handle, handle)
        });
        self.scheduled_seeks.retain(|seek| seek.item_id != item_id);
        self.prepared_launch_epochs
            .retain(|(bound_id, _)| *bound_id != item_id);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use kithara_audio::SeekOutcome;
    use kithara_test_utils::kithara;
    use kithara_warp::{
        PresentationFrontier, RenderContext, RenderPublisher, SessionEpoch, SessionFrame,
    };
    use ringbuf::traits::Consumer;

    use super::*;
    use crate::bridge::PreparedLaunchIdentity;

    struct CountSeek(AtomicUsize);

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
    fn scheduled_track_seek_begins_inside_the_preparation_window() {
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

        control.service_scheduled_seeks(448);
        assert_eq!(seek.0.load(Ordering::Relaxed), 0);
        assert!(inputs.cmd_rx.try_pop().is_none());

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
        control.service_scheduled_seeks(448);
        assert_eq!(prepared_seek.0.load(Ordering::Relaxed), 0);
        assert!(control.set_prepared_launch_armed(prepared_item, true));
        control.service_scheduled_seeks(448);
        assert_eq!(prepared_seek.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            inputs.cmd_rx.try_pop(),
            Some(PlayerCmd::ScheduleSeek {
                item_id,
                disposition: ScheduledSeekDisposition::PreparedLaunch(_),
                ..
            }) if item_id == prepared_item
        ));

        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1_600)
                .output(SessionFrame::new(1_600))
                .build(),
        );
        control.service_scheduled_seeks(448);
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
        control.service_scheduled_seeks(448);
        assert_eq!(seek.0.load(Ordering::Relaxed), 0);
        assert!(inputs.cmd_rx.try_pop().is_none());
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

        control.service_scheduled_seeks(448);

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
        while control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: None,
            })
            .is_ok()
        {}
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::SeekOnly {
                activation: SessionFrame::new(1_000),
            },
        );

        control.service_scheduled_seeks(448);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(inputs.cmd_rx.try_pop().is_some());

        control.service_scheduled_seeks(448);
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
        while control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: None,
            })
            .is_ok()
        {}
        control.schedule_track_seek(
            item,
            Duration::from_secs(3),
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: SessionFrame::new(1_000),
                warp_map: kithara_warp::WarpMapRevision::first(),
            }),
        );

        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(448);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(inputs.cmd_rx.try_pop().is_some());

        control.service_scheduled_seeks(448);
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
        while control
            .cmd_tx
            .try_push(PlayerCmd::SetFadeDuration(1.0))
            .is_ok()
        {}

        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(448);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(
            std::iter::from_fn(|| inputs.cmd_rx.try_pop())
                .all(|command| matches!(command, PlayerCmd::SetFadeDuration(1.0)))
        );

        control.service_scheduled_seeks(448);
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
        while control
            .cmd_tx
            .try_push(PlayerCmd::SetFadeDuration(1.0))
            .is_ok()
        {}

        assert!(control.set_prepared_launch_armed(item, true));
        control.service_scheduled_seeks(448);
        assert_eq!(seek.0.load(Ordering::Relaxed), 1);
        assert!(control.set_prepared_launch_armed(item, false));
        while inputs.cmd_rx.try_pop().is_some() {}

        control.service_scheduled_seeks(448);
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
        control.service_scheduled_seeks(448);

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

        while control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: None,
            })
            .is_ok()
        {}
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
        scheduled_seeks: Vec::new(),
        prepared_launch_epochs: Vec::new(),
        render: RenderBindings::default(),
    };
    (inputs, control)
}
