use kithara_audio::SeekBegin;
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
use kithara_sync::LoadGeneration;
use kithara_warp::{RenderReader, RenderSnapshot, WarpMapRevision};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};
use smallvec::SmallVec;

use super::PlaybackShared;
use crate::{
    bridge::{
        PlayerCmd, PlayerNotification, SharedEq, SyncReceiptTx,
        sync::{SyncReturn, SyncTicket},
    },
    rt::{PlayerNodeProcessor, track::PlayerTrack},
};

/// RT-owned channel halves and playback atomics for one player node.
#[non_exhaustive]
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub struct NodeInputs {
    pub(crate) playback: Arc<PlaybackShared>,
    pub(crate) cmd_rx: HeapCons<PlayerCmd>,
    pub(crate) notif_tx: HeapProd<PlayerNotification>,
    pub(crate) trash_tx: HeapProd<PlayerTrack>,
    #[field(with, option_set_some)]
    pub(crate) sync_receipts: Option<SyncReceiptTx>,
    pub(crate) sync_rx: HeapCons<SyncTicket>,
    pub(crate) sync_return_tx: HeapProd<SyncReturn>,
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
    pub playback: Arc<PlaybackShared>,
    pub notif_rx: HeapCons<PlayerNotification>,
    pub trash_rx: HeapCons<PlayerTrack>,
    pub cmd_tx: HeapProd<PlayerCmd>,
    pub eq: SharedEq,
    pub(crate) sync_tx: HeapProd<SyncTicket>,
    pub(crate) sync_return_rx: HeapCons<SyncReturn>,
    render: RenderBindings,
    seek: SeekBindings,
}

#[derive(Default)]
struct SeekBindings(SmallVec<[SeekBinding; SLOT_TRACKS]>);

type SeekBinding = (TrackId, Arc<dyn SeekBegin>);

#[derive(Default)]
struct RenderBindings(SmallVec<[RenderBinding; SLOT_TRACKS]>);

type RenderBinding = (
    TrackId,
    LoadGeneration,
    Option<WarpMapRevision>,
    RenderReader,
);

const SLOT_TRACKS: usize = PlayerNodeProcessor::MAX_TRACKS;

impl SlotControl {
    /// Begin a seek on every track this slot holds, off the audio thread.
    pub fn begin_seek(&self, position: Duration) {
        for (_, handle) in &self.seek.0 {
            handle.begin(position);
        }
    }

    pub(crate) fn bind_render(
        &mut self,
        item_id: TrackId,
        load: LoadGeneration,
        reader: RenderReader,
    ) {
        self.render.0.push((item_id, load, None, reader));
    }

    pub(crate) fn bind_sync_resource(
        &mut self,
        item_id: TrackId,
        load: LoadGeneration,
        map: WarpMapRevision,
        seek: Option<Arc<dyn SeekBegin>>,
        reader: RenderReader,
    ) {
        if let Some(seek) = seek {
            self.bind_seek(item_id, seek);
        }
        self.render.0.push((item_id, load, Some(map), reader));
    }

    /// Read only the newest binding for this item, including its load identity.
    pub(crate) fn render_binding(
        &self,
        item_id: TrackId,
    ) -> Option<(LoadGeneration, Option<RenderSnapshot>)> {
        let active = self.playback.active_sync_map.load(Ordering::Acquire);
        let active_item = self.render.0.iter().find_map(|(id, _, map, _)| {
            map.is_some_and(|map| u64::from(map) == active)
                .then_some(*id)
        });
        self.render
            .0
            .iter()
            .rev()
            .find(|(bound_id, _, map, _)| {
                *bound_id == item_id
                    && if active_item == Some(item_id) {
                        map.is_some_and(|map| u64::from(map) == active)
                    } else {
                        map.is_none()
                    }
            })
            .map(|(_, load, _, reader)| (*load, reader.load()))
    }

    /// Record the control half of a track's seek path.
    pub fn bind_seek(&mut self, item_id: TrackId, handle: Arc<dyn SeekBegin>) {
        self.seek.0.push((item_id, handle));
    }

    pub(crate) fn latest_render_snapshot(&self) -> Option<RenderSnapshot> {
        let active = self.playback.active_sync_map.load(Ordering::Acquire);
        let active_item = self.render.0.iter().find_map(|(id, _, map, _)| {
            map.is_some_and(|map| u64::from(map) == active)
                .then_some(*id)
        });
        self.render
            .0
            .iter()
            .filter(|(item_id, _, map, _)| {
                if map.is_some() {
                    map.is_some_and(|map| u64::from(map) == active)
                } else {
                    active_item != Some(*item_id)
                }
            })
            .filter_map(|(_, _, _, reader)| reader.load())
            .max_by_key(|snapshot| {
                let context = snapshot.context();
                (
                    u64::from(context.output().session_epoch()),
                    i64::from(context.output().output_frames().end),
                )
            })
    }

    pub(crate) fn unbind_render(&mut self, item_id: TrackId, reader: &RenderReader) {
        self.render.0.retain(|(bound_id, _, _, bound_reader)| {
            *bound_id != item_id || bound_reader != reader
        });
    }

    /// Forget the exact resource generation returned by the processor.
    pub fn unbind_seek(&mut self, item_id: TrackId, handle: &Arc<dyn SeekBegin>) {
        self.seek.0.retain(|(bound_id, bound_handle)| {
            *bound_id != item_id || !Arc::ptr_eq(bound_handle, handle)
        });
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
    let (sync_tx, sync_rx) = HeapRb::<SyncTicket>::new(1).split();
    let (sync_return_tx, sync_return_rx) = HeapRb::<SyncReturn>::new(2).split();
    let playback = Arc::new(PlaybackShared::default());

    let inputs = NodeInputs {
        cmd_rx,
        notif_tx,
        trash_tx,
        sync_receipts: None,
        sync_rx,
        sync_return_tx,
        playback: Arc::clone(&playback),
    };
    let control = SlotControl {
        playback,
        notif_rx,
        trash_rx,
        cmd_tx,
        eq,
        sync_tx,
        sync_return_rx,
        seek: SeekBindings::default(),
        render: RenderBindings::default(),
    };
    (inputs, control)
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::atomic::AtomicUsize};

    use kithara_audio::SeekOutcome;
    use kithara_signal::{OutputContext, SessionEpoch, SessionFrame};
    use kithara_test_utils::kithara;
    use kithara_warp::{PresentationFrontier, RenderContext, RenderPublisher};

    use super::*;

    struct CountSeek(Arc<AtomicUsize>);

    impl SeekBegin for CountSeek {
        fn begin(&self, position: Duration) -> SeekOutcome {
            self.0.fetch_add(1, Ordering::Relaxed);
            SeekOutcome::Landed {
                target: position,
                landed_at: position,
            }
        }
    }

    fn published(end: i64) -> RenderReader {
        let publisher = RenderPublisher::default();
        let reader = publisher.reader();
        let output = OutputContext::new(
            SessionFrame::new(0)..SessionFrame::new(end),
            NonZeroU32::new(48_000).expect("fixture rate"),
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture output context");
        let context = RenderContext::new_linear(output, None).expect("fixture render context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(u64::try_from(end).expect("positive fixture frame"))
                .output(SessionFrame::new(end))
                .build(),
        );
        reader
    }

    #[kithara::test]
    fn resident_lookup_does_not_use_outgoing_or_prior_load_render() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let outgoing_id = TrackId::allocate();
        let resident_id = TrackId::allocate();
        let first = LoadGeneration::first();
        let second = first.checked_next().expect("fixture generation");
        control.bind_render(outgoing_id, first, published(512));
        control.bind_render(resident_id, second, published(128));

        let (bound, snapshot) = control
            .render_binding(resident_id)
            .expect("resident binding");
        assert_eq!(bound, second);
        assert_eq!(
            snapshot
                .expect("resident context")
                .context()
                .output()
                .output_frames()
                .end,
            SessionFrame::new(128)
        );
        assert_eq!(
            control
                .latest_render_snapshot()
                .expect("outgoing context")
                .context()
                .output()
                .output_frames()
                .end,
            SessionFrame::new(512)
        );

        let unpublished = RenderPublisher::default();
        control.bind_render(resident_id, first, unpublished.reader());
        assert!(matches!(control.render_binding(resident_id), Some((load, None)) if load == first));
    }

    #[kithara::test]
    fn mapped_resident_remains_selected_when_unrelated_successor_preloads() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let resident = TrackId::allocate();
        let successor = TrackId::allocate();
        let load = LoadGeneration::first();
        let first_map = WarpMapRevision::first();
        control.bind_render(resident, load, published(512));
        control.bind_sync_resource(resident, load, first_map, None, published(128));
        control.bind_render(successor, load, RenderPublisher::default().reader());

        let before = control
            .render_binding(resident)
            .expect("resident binding")
            .1
            .expect("ordinary resident snapshot");
        assert_eq!(
            before.context().output().output_frames().end,
            SessionFrame::new(512)
        );

        control
            .playback
            .active_sync_map
            .store(u64::from(first_map), Ordering::Release);
        let after = control
            .render_binding(resident)
            .expect("mapped resident binding")
            .1
            .expect("mapped resident snapshot");
        assert_eq!(
            after.context().output().output_frames().end,
            SessionFrame::new(128)
        );
        assert!(matches!(control.render_binding(successor), Some((bound, None)) if bound == load));
        assert_eq!(
            control
                .latest_render_snapshot()
                .expect("mapped sounding snapshot")
                .context()
                .output()
                .output_frames()
                .end,
            SessionFrame::new(128)
        );
    }

    #[kithara::test]
    fn pending_reissued_map_does_not_shadow_the_sounding_reader() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let resident = TrackId::allocate();
        let load = LoadGeneration::first();
        let sounding = WarpMapRevision::first();
        let pending = sounding.checked_next().expect("fixture map revision");
        control.bind_render(resident, load, published(64));
        control.bind_sync_resource(resident, load, sounding, None, published(128));
        control
            .playback
            .active_sync_map
            .store(u64::from(sounding), Ordering::Release);
        control.bind_sync_resource(resident, load, pending, None, published(256));

        let observed = control
            .render_binding(resident)
            .expect("resident binding")
            .1
            .expect("sounding map");
        assert_eq!(
            observed.context().output().output_frames().end,
            SessionFrame::new(128)
        );

        control
            .playback
            .active_sync_map
            .store(u64::from(pending), Ordering::Release);
        let observed = control
            .render_binding(resident)
            .expect("resident binding")
            .1
            .expect("newly consumed map");
        assert_eq!(
            observed.context().output().output_frames().end,
            SessionFrame::new(256)
        );
    }

    #[kithara::test]
    fn seek_after_sync_activation_reaches_the_staged_reader_until_return() {
        let (_, mut control) = slot_channels(SharedEq::new(0));
        let resident = TrackId::allocate();
        let load = LoadGeneration::first();
        let map = WarpMapRevision::first();
        let begins = Arc::new(AtomicUsize::new(0));
        let handle: Arc<dyn SeekBegin> = Arc::new(CountSeek(Arc::clone(&begins)));
        let reader = published(128);
        control.bind_sync_resource(resident, load, map, Some(Arc::clone(&handle)), reader);
        control
            .playback
            .active_sync_map
            .store(u64::from(map), Ordering::Release);

        control.begin_seek(Duration::from_secs(1));
        assert_eq!(begins.load(Ordering::Relaxed), 1);

        control.unbind_seek(resident, &handle);
        control.begin_seek(Duration::from_secs(2));
        assert_eq!(begins.load(Ordering::Relaxed), 1);
    }
}
