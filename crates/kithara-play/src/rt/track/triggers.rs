use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapProd, traits::Producer};

use crate::bridge::PlayerNotification;

#[derive(Default)]
pub(crate) struct TrackTriggers {
    notified_prefetch_requested: bool,
    notified_track_requested: bool,
}

impl TrackTriggers {
    pub(crate) fn check(
        &mut self,
        notification_tx: &mut HeapProd<PlayerNotification>,
        track: TriggerTrack<'_>,
        input: TriggerInput,
    ) {
        let block_frames_f32: f32 = input.block_frames.as_();
        let sr_f32: f32 = input.sample_rate.as_();
        let block_seconds = block_frames_f32 / sr_f32;
        let fade_threshold = input.fade_duration;
        let prefetch_threshold = input.prefetch_duration.max(input.fade_duration) + block_seconds;

        if let Some(frames_until_eof) = input.frames_until_eof {
            let frames_f32: f32 = frames_until_eof.as_();
            let remaining = frames_f32 / sr_f32;
            if remaining <= prefetch_threshold {
                self.emit_track_requested(notification_tx);
            }
            if remaining <= fade_threshold + block_seconds {
                self.emit_handover_requested(notification_tx, track);
            }
            return;
        }

        if input.duration <= 0.0 {
            return;
        }

        let pos: f32 = input.position.as_();
        let dur: f32 = input.duration.as_();

        if pos + prefetch_threshold >= dur {
            self.emit_track_requested(notification_tx);
        }
        if pos + fade_threshold >= dur {
            self.emit_handover_requested(notification_tx, track);
        }
    }

    pub(crate) fn emit_handover_requested(
        &mut self,
        notification_tx: &mut HeapProd<PlayerNotification>,
        track: TriggerTrack<'_>,
    ) {
        if self.notified_track_requested {
            return;
        }

        if notification_tx
            .try_push(PlayerNotification::HandoverRequested {
                src: Arc::clone(track.src),
                item_id: track.item_id,
            })
            .is_ok()
        {
            self.notified_track_requested = true;
        }
    }

    fn emit_track_requested(&mut self, notification_tx: &mut HeapProd<PlayerNotification>) {
        if self.notified_prefetch_requested {
            return;
        }

        if notification_tx
            .try_push(PlayerNotification::Requested)
            .is_ok()
        {
            self.notified_prefetch_requested = true;
        }
    }

    pub(crate) const fn mark_prefetch_requested(&mut self) {
        self.notified_prefetch_requested = true;
    }

    pub(crate) const fn reset(&mut self) {
        self.notified_prefetch_requested = false;
        self.notified_track_requested = false;
    }
}

/// The track a trigger speaks for.
///
/// A handover request means "*this* track is about to end". Without the
/// track it is only "something is about to end", and a consumer that has
/// already moved its cursor on reads it as a statement about whatever it
/// holds now.
#[derive(Clone, Copy)]
pub(crate) struct TriggerTrack<'a> {
    pub(crate) src: &'a Arc<str>,
    pub(crate) item_id: TrackId,
}

#[derive(Clone, Copy)]
pub(crate) struct TriggerInput {
    pub(crate) frames_until_eof: Option<usize>,
    pub(crate) fade_duration: f32,
    pub(crate) prefetch_duration: f32,
    pub(crate) duration: f64,
    pub(crate) position: f64,
    pub(crate) sample_rate: u32,
    pub(crate) block_frames: usize,
}
