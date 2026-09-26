use js_sys::Reflect;
use kithara::{
    events::{Envelope, EventReceiver},
    platform::{
        time::{Duration, sleep},
        tokio::{sync::broadcast::error::RecvError, task::spawn as task_spawn},
    },
    queue::QueueEvent,
};
use wasm_bindgen::JsValue;
use web_sys::{BroadcastChannel, console};

use super::{encode::encode, encode_item::encode_item_event};
use crate::{
    core::event_set::{ItemBusEvent, QueueBusEvent},
    pools::FfiQueueControl,
    types::FfiPlayerEvent,
};

pub(crate) mod consts {
    /// `BroadcastChannel` name carrying structured player events from the
    /// worker to the main-thread [`router`](crate::web::observer::router).
    pub(crate) const EVENT_CHANNEL: &str = "kithara-events";
}

/// Subscribe to the queue's event bus inside the worker and forward every
/// translated [`FfiPlayerEvent`] to the main thread over
/// [`EVENT_CHANNEL`](consts::EVENT_CHANNEL). Spawned from
/// [`worker_main`](crate::web::worker::worker_main).
pub(crate) fn spawn(queue: &FfiQueueControl) {
    let rx = queue.subscribe::<QueueBusEvent>();
    let item_rx = queue.subscribe::<ItemBusEvent>();
    task_spawn(async move {
        run_items(item_rx).await;
    });
    task_spawn(async move {
        run(rx).await;
    });
    spawn_duration_poll(queue);
}

/// Emit [`FfiPlayerEvent::DurationChanged`] whenever the current track's
/// duration changes. `DurationChanged` is not a raw bus event: the native
/// bridge derives it by polling [`FfiQueueControl::duration_seconds`], so the
/// worker must do the same here. Without this the JS control surface never
/// learns the track length and the seek slider has no range.
fn spawn_duration_poll(queue: &FfiQueueControl) {
    /// Poll cadence for the derived `DurationChanged` event, in milliseconds.
    const DURATION_POLL_MS: u64 = 250;

    let queue = queue.clone();
    task_spawn(async move {
        let Ok(channel) = BroadcastChannel::new(consts::EVENT_CHANNEL) else {
            return;
        };
        let mut last: Option<f64> = None;
        loop {
            if queue.is_closed() {
                break;
            }
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

async fn run(mut rx: EventReceiver<QueueBusEvent>) {
    let Ok(channel) = BroadcastChannel::new(consts::EVENT_CHANNEL) else {
        console::warn_1(&JsValue::from_str(
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
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
}

/// Keep the main-thread current-track read-back
/// ([`WorkerBridge::current_track_id`](crate::web::bridge::WorkerBridge))
/// in sync by mirroring the worker's current-track cursor into the shared
/// atomic on every relevant queue event.
fn mirror_current_track(event: &QueueBusEvent) {
    match event {
        QueueBusEvent::Queue(QueueEvent::CurrentTrackChanged { id }) => {
            crate::web::bridge::set_current_track_id(*id);
        }
        QueueBusEvent::Queue(QueueEvent::QueueEnded) => {
            crate::web::bridge::set_current_track_id(None);
        }
        _ => {}
    }
}

fn to_ffi(event: &QueueBusEvent) -> Option<FfiPlayerEvent> {
    match event {
        QueueBusEvent::Player(pe) => FfiPlayerEvent::try_from(pe).ok(),
        QueueBusEvent::Queue(qe) => FfiPlayerEvent::try_from(qe).ok(),
        _ => FfiPlayerEvent::try_from(event).ok(),
    }
}

async fn run_items(mut rx: EventReceiver<ItemBusEvent>) {
    let Ok(channel) = BroadcastChannel::new(consts::EVENT_CHANNEL) else {
        return;
    };
    loop {
        match rx.recv().await {
            Ok(Envelope { event, meta, .. }) => {
                if let Ok(item_ffi) = crate::types::FfiItemEvent::try_from(&event)
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
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
}
