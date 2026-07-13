use std::rc::Rc;

use js_sys::Reflect;
use kithara_events::{Envelope, Event, EventReceiver, QueueEvent};
use kithara_platform::{
    time::{Duration, sleep},
    tokio::{sync::broadcast, task::spawn as task_spawn},
};
use kithara_play::PlayerEvent;
use kithara_queue::Queue;
use wasm_bindgen::JsValue;
use web_sys::BroadcastChannel;

use super::{encode::encode, encode_item::encode_item_event};
use crate::types::{FfiAdvanceReason, FfiPlayerEvent, FfiRepeatMode, FfiTrackStatus};

/// `BroadcastChannel` name carrying structured player events from the
/// worker to the main-thread [`router`](crate::web::observer::router).
pub(crate) const EVENT_CHANNEL: &str = "kithara-events";

/// Subscribe to the queue's event bus inside the worker and forward every
/// translated [`FfiPlayerEvent`] to the main thread over
/// [`EVENT_CHANNEL`]. Spawned from
/// [`worker_main`](crate::web::worker::worker_main).
pub(crate) fn spawn(queue: &Rc<Queue>) {
    let rx = queue.subscribe();
    task_spawn(async move {
        run(rx).await;
    });
    spawn_duration_poll(queue);
}

/// Emit [`FfiPlayerEvent::DurationChanged`] whenever the current track's
/// duration changes. `DurationChanged` is not a raw bus event: the native
/// bridge derives it by polling [`Queue::duration_seconds`], so the worker
/// must do the same here. Without this the JS control surface never learns
/// the track length and the seek slider has no range.
fn spawn_duration_poll(queue: &Rc<Queue>) {
    /// Poll cadence for the derived `DurationChanged` event, in milliseconds.
    const DURATION_POLL_MS: u64 = 250;

    let queue = Rc::clone(queue);
    task_spawn(async move {
        let Ok(channel) = BroadcastChannel::new(EVENT_CHANNEL) else {
            return;
        };
        let mut last: Option<f64> = None;
        loop {
            let current = queue.duration_seconds();
            if current != last
                && let Some(seconds) = current
            {
                let _ = channel.post_message(&encode(&FfiPlayerEvent::DurationChanged { seconds }));
                last = current;
            }
            sleep(Duration::from_millis(DURATION_POLL_MS)).await;
        }
    });
}

async fn run(mut rx: EventReceiver) {
    let Ok(channel) = BroadcastChannel::new(EVENT_CHANNEL) else {
        web_sys::console::warn_1(&JsValue::from_str(
            "kithara: BroadcastChannel unavailable in worker; event bridge disabled",
        ));
        return;
    };
    loop {
        match rx.recv().await {
            Ok(Envelope { event, meta, .. }) => {
                mirror_current_track(&event);
                if let Some(ffi) = to_ffi(&event) {
                    let _ = channel.post_message(&encode(&ffi));
                }
                if let Some(item_ffi) = crate::core::convert::item_event_to_ffi(&event)
                    && let Some(track) = meta.track
                {
                    let msg = encode_item_event(&item_ffi);
                    let _ = Reflect::set(
                        &msg,
                        &JsValue::from_str("scope"),
                        &JsValue::from_str("item"),
                    );
                    let _ = Reflect::set(
                        &msg,
                        &JsValue::from_str("track_id"),
                        &JsValue::from_f64(num_traits::cast(track.as_u64()).unwrap_or(0.0)),
                    );
                    let _ = channel.post_message(&msg);
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Keep the main-thread current-track read-back
/// ([`WorkerBridge::current_track_id`](crate::web::bridge::WorkerBridge))
/// in sync by mirroring the worker's current-track cursor into the shared
/// atomic on every relevant queue event.
fn mirror_current_track(event: &Event) {
    match event {
        Event::Queue(QueueEvent::CurrentTrackChanged { id }) => {
            crate::web::bridge::set_current_track_id(*id);
        }
        Event::Queue(QueueEvent::QueueEnded) => {
            crate::web::bridge::set_current_track_id(None);
        }
        _ => {}
    }
}

fn to_ffi(event: &Event) -> Option<FfiPlayerEvent> {
    match event {
        Event::Player(pe) => player_event_to_ffi(pe),
        Event::Queue(qe) => queue_event_to_ffi(qe),
        _ => crate::core::convert::player_event_to_ffi(event),
    }
}

fn player_event_to_ffi(event: &PlayerEvent) -> Option<FfiPlayerEvent> {
    Some(match event {
        PlayerEvent::RateChanged { rate } => FfiPlayerEvent::RateChanged { rate: *rate },
        PlayerEvent::StatusChanged { status } => FfiPlayerEvent::StatusChanged {
            status: (*status).into(),
        },
        PlayerEvent::TimeControlStatusChanged { status, .. } => {
            FfiPlayerEvent::TimeControlStatusChanged {
                status: (*status).into(),
            }
        }
        PlayerEvent::VolumeChanged { volume } => FfiPlayerEvent::VolumeChanged { volume: *volume },
        PlayerEvent::MuteChanged { muted } => FfiPlayerEvent::MuteChanged { muted: *muted },
        PlayerEvent::ItemDidPlayToEnd { .. } => FfiPlayerEvent::ItemDidPlayToEnd,
        _ => return None,
    })
}

fn queue_event_to_ffi(event: &QueueEvent) -> Option<FfiPlayerEvent> {
    Some(match event {
        QueueEvent::TrackAdded { id, index } => FfiPlayerEvent::TrackAdded {
            item_id: *id,
            index: *index as u64,
        },
        QueueEvent::TrackRemoved { id } => FfiPlayerEvent::TrackRemoved { item_id: *id },
        QueueEvent::TrackStatusChanged { id, status } => FfiPlayerEvent::TrackStatusChanged {
            item_id: *id,
            status: FfiTrackStatus::from(status.clone()),
        },
        QueueEvent::CurrentTrackChanged { id } => {
            FfiPlayerEvent::CurrentItemChanged { item_id: *id }
        }
        QueueEvent::CurrentTrackAdvance { id, reason } => FfiPlayerEvent::CurrentItemAdvanced {
            item_id: *id,
            reason: FfiAdvanceReason::from(*reason),
        },
        QueueEvent::QueueEnded => FfiPlayerEvent::QueueEnded,
        QueueEvent::TrackLoadFailed {
            id,
            reason,
            auto_skipped,
        } => FfiPlayerEvent::TrackLoadFailed {
            item_id: *id,
            reason: reason.clone(),
            auto_skipped: *auto_skipped,
        },
        QueueEvent::CrossfadeStarted { duration_seconds } => FfiPlayerEvent::CrossfadeStarted {
            duration_seconds: *duration_seconds,
        },
        QueueEvent::CrossfadeDurationChanged { seconds } => {
            FfiPlayerEvent::CrossfadeDurationChanged { seconds: *seconds }
        }
        QueueEvent::RepeatModeChanged { mode } => FfiPlayerEvent::RepeatModeChanged {
            mode: FfiRepeatMode::from(*mode),
        },
        QueueEvent::NextTrackReady { id, index } => FfiPlayerEvent::NextTrackReady {
            item_id: *id,
            index: *index as u64,
        },
        _ => return None,
    })
}
