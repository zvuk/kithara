use std::rc::Rc;

use js_sys::{Object, Reflect};
use kithara_events::{Envelope, Event, EventReceiver, QueueEvent};
use kithara_platform::{
    time::{Duration, sleep},
    tokio::{sync::broadcast, task::spawn as task_spawn},
};
use kithara_play::PlayerEvent;
use kithara_queue::Queue;
use wasm_bindgen::JsValue;
use web_sys::BroadcastChannel;

use crate::types::{FfiPlayerEvent, FfiPlayerStatus, FfiTimeControlStatus, FfiTrackStatus};

/// `BroadcastChannel` name carrying structured player events from the
/// worker to the main-thread [`router`](crate::web::observer::router).
pub(crate) const EVENT_CHANNEL: &str = "kithara-events";

/// Discriminator key in the marshalled event object.
const KIND: &str = "kind";

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
            Ok(Envelope { event, .. }) => {
                mirror_current_track(&event);
                if let Some(ffi) = to_ffi(&event) {
                    let _ = channel.post_message(&encode(&ffi));
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
        _ => None,
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
        QueueEvent::TrackStatusChanged { id, status } => FfiPlayerEvent::TrackStatusChanged {
            item_id: *id,
            status: FfiTrackStatus::from(status.clone()),
        },
        QueueEvent::CurrentTrackChanged { id } => {
            FfiPlayerEvent::CurrentItemChanged { item_id: *id }
        }
        QueueEvent::QueueEnded => FfiPlayerEvent::QueueEnded,
        QueueEvent::CrossfadeStarted { duration_seconds } => FfiPlayerEvent::CrossfadeStarted {
            duration_seconds: *duration_seconds,
        },
        QueueEvent::CrossfadeDurationChanged { seconds } => {
            FfiPlayerEvent::CrossfadeDurationChanged { seconds: *seconds }
        }
        _ => return None,
    })
}

fn set_str(obj: &Object, key: &str, val: &str) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(val));
}

fn set_f64(obj: &Object, key: &str, val: f64) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_f64(val));
}

fn set_bool(obj: &Object, key: &str, val: bool) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_bool(val));
}

fn set_opt_id(obj: &Object, key: &str, id: Option<kithara_events::TrackId>) {
    if let Some(id) = id {
        set_f64(obj, key, num_traits::cast(id.as_u64()).unwrap_or(0.0));
    }
}

/// Marshal an [`FfiPlayerEvent`] into a plain JS object the
/// [`router`](crate::web::observer::router) decodes, and the
/// [`shim`](crate::web::observer::shim) hands to a JS callback.
pub(crate) fn encode(event: &FfiPlayerEvent) -> JsValue {
    let obj = Object::new();
    match event {
        FfiPlayerEvent::TimeChanged { seconds } => {
            set_str(&obj, KIND, "TimeChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiPlayerEvent::RateChanged { rate } => {
            set_str(&obj, KIND, "RateChanged");
            set_f64(&obj, "rate", f64::from(*rate));
        }
        FfiPlayerEvent::CurrentItemChanged { item_id } => {
            set_str(&obj, KIND, "CurrentItemChanged");
            set_opt_id(&obj, "item_id", *item_id);
        }
        FfiPlayerEvent::StatusChanged { status } => {
            set_str(&obj, KIND, "StatusChanged");
            set_f64(&obj, "status", player_status_code(*status));
        }
        FfiPlayerEvent::TimeControlStatusChanged { status } => {
            set_str(&obj, KIND, "TimeControlStatusChanged");
            set_f64(&obj, "status", time_control_code(*status));
        }
        FfiPlayerEvent::Error { error } => {
            set_str(&obj, KIND, "Error");
            set_str(&obj, "error", error);
        }
        FfiPlayerEvent::DurationChanged { seconds } => {
            set_str(&obj, KIND, "DurationChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiPlayerEvent::BufferedDurationChanged { seconds } => {
            set_str(&obj, KIND, "BufferedDurationChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiPlayerEvent::VolumeChanged { volume } => {
            set_str(&obj, KIND, "VolumeChanged");
            set_f64(&obj, "volume", f64::from(*volume));
        }
        FfiPlayerEvent::MuteChanged { muted } => {
            set_str(&obj, KIND, "MuteChanged");
            set_bool(&obj, "muted", *muted);
        }
        FfiPlayerEvent::ItemDidPlayToEnd => set_str(&obj, KIND, "ItemDidPlayToEnd"),
        FfiPlayerEvent::ItemDidFail { item_id } => {
            set_str(&obj, KIND, "ItemDidFail");
            set_opt_id(&obj, "item_id", *item_id);
        }
        FfiPlayerEvent::TrackStatusChanged { item_id, status } => {
            set_str(&obj, KIND, "TrackStatusChanged");
            set_f64(
                &obj,
                "item_id",
                num_traits::cast(item_id.as_u64()).unwrap_or(0.0),
            );
            encode_track_status(&obj, status);
        }
        FfiPlayerEvent::QueueEnded => set_str(&obj, KIND, "QueueEnded"),
        FfiPlayerEvent::CrossfadeStarted { duration_seconds } => {
            set_str(&obj, KIND, "CrossfadeStarted");
            set_f64(&obj, "seconds", f64::from(*duration_seconds));
        }
        FfiPlayerEvent::CrossfadeDurationChanged { seconds } => {
            set_str(&obj, KIND, "CrossfadeDurationChanged");
            set_f64(&obj, "seconds", f64::from(*seconds));
        }
    }
    obj.into()
}

fn encode_track_status(obj: &Object, status: &FfiTrackStatus) {
    let (code, reason) = match status {
        FfiTrackStatus::Pending => (0.0, None),
        FfiTrackStatus::Loading => (1.0, None),
        FfiTrackStatus::Slow => (2.0, None),
        FfiTrackStatus::Loaded => (3.0, None),
        FfiTrackStatus::Failed { reason } => (4.0, Some(reason.clone())),
        FfiTrackStatus::Consumed => (5.0, None),
        FfiTrackStatus::Cancelled => (6.0, None),
    };
    set_f64(obj, "status", code);
    if let Some(reason) = reason {
        set_str(obj, "reason", &reason);
    }
}

fn player_status_code(status: FfiPlayerStatus) -> f64 {
    match status {
        FfiPlayerStatus::Unknown => 0.0,
        FfiPlayerStatus::ReadyToPlay => 1.0,
        FfiPlayerStatus::Failed => 2.0,
    }
}

fn time_control_code(status: FfiTimeControlStatus) -> f64 {
    match status {
        FfiTimeControlStatus::Paused => 0.0,
        FfiTimeControlStatus::WaitingToPlay => 1.0,
        FfiTimeControlStatus::Playing => 2.0,
    }
}
