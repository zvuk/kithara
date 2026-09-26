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
use kithara_warp::{RenderReader, RenderSnapshot};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};
use smallvec::SmallVec;

use super::PlaybackShared;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, SharedEq},
    rt::{PlayerNodeProcessor, track::PlayerTrack},
};

/// RT-owned channel halves and playback atomics for one player node.
#[non_exhaustive]
pub struct NodeInputs {
    pub(crate) playback: Arc<PlaybackShared>,
    pub(crate) cmd_rx: HeapCons<PlayerCmd>,
    pub(crate) notif_tx: HeapProd<PlayerNotification>,
    pub(crate) trash_tx: HeapProd<PlayerTrack>,
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
    render: RenderBindings,
    seek: SeekBindings,
}

#[derive(Default)]
struct SeekBindings(SmallVec<[SeekBinding; SLOT_TRACKS]>);

type SeekBinding = (TrackId, Arc<dyn SeekBegin>);

#[derive(Default)]
struct RenderBindings(SmallVec<[RenderBinding; SLOT_TRACKS]>);

type RenderBinding = (TrackId, LoadGeneration, RenderReader);

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
        self.render.0.push((item_id, load, reader));
    }

    /// Read only the newest binding for this item, including its load identity.
    pub(crate) fn render_binding(
        &self,
        item_id: TrackId,
    ) -> Option<(LoadGeneration, Option<RenderSnapshot>)> {
        self.render
            .0
            .iter()
            .rev()
            .find(|(bound_id, _, _)| *bound_id == item_id)
            .map(|(_, load, reader)| (*load, reader.load()))
    }

    /// Record the control half of a track's seek path.
    pub fn bind_seek(&mut self, item_id: TrackId, handle: Arc<dyn SeekBegin>) {
        self.seek.0.push((item_id, handle));
    }

    pub(crate) fn latest_render_snapshot(&self) -> Option<RenderSnapshot> {
        self.render
            .0
            .iter()
            .filter_map(|(_, _, reader)| reader.load())
            .max_by_key(|snapshot| {
                let context = snapshot.context();
                (
                    u64::from(context.output().session_epoch()),
                    i64::from(context.output().output_frames().end),
                )
            })
    }

    pub(crate) fn unbind_render(&mut self, item_id: TrackId, reader: &RenderReader) {
        self.render
            .0
            .retain(|(bound_id, _, bound_reader)| *bound_id != item_id || bound_reader != reader);
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
    let playback = Arc::new(PlaybackShared::default());

    let inputs = NodeInputs {
        cmd_rx,
        notif_tx,
        trash_tx,
        playback: Arc::clone(&playback),
    };
    let control = SlotControl {
        playback,
        notif_rx,
        trash_rx,
        cmd_tx,
        eq,
        seek: SeekBindings::default(),
        render: RenderBindings::default(),
    };
    (inputs, control)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::{OutputContext, SessionEpoch, SessionFrame};
    use kithara_test_utils::kithara;
    use kithara_warp::{PresentationFrontier, RenderContext, RenderPublisher};

    use super::*;

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
}
