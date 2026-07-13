use kithara_events::TrackId;
use kithara_platform::sync::{Arc, Mutex};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use web_sys::{BroadcastChannel, MessageEvent};

use super::{decode::decode, decode_item::decode_item_event, marshal::get_id_req};
use crate::{
    item::AudioPlayerItem,
    observer::{ItemObserver, PlayerObserver},
    types::{FfiItemEvent, FfiItemStatus, FfiPlayerEvent, FfiTrackStatus},
    web::observer::source::EVENT_CHANNEL,
};

type QueueView = Vec<(TrackId, Arc<AudioPlayerItem>)>;

pub(crate) fn install(observer: Arc<dyn PlayerObserver>, queue_view: Arc<Mutex<QueueView>>) {
    let Ok(channel) = BroadcastChannel::new(EVENT_CHANNEL) else {
        web_sys::console::warn_1(&JsValue::from_str(
            "kithara: BroadcastChannel unavailable; player observer disabled",
        ));
        return;
    };
    let closure = Closure::wrap(Box::new(move |ev: MessageEvent| {
        let data = ev.data();
        if js_sys::Reflect::get(&data, &JsValue::from_str("scope"))
            .ok()
            .and_then(|value| value.as_string())
            .as_deref()
            == Some("item")
        {
            route_item_message(&queue_view, &data);
            return;
        }
        let Some(event) = decode(&data) else {
            return;
        };
        route_to_item(&queue_view, &event);
        observer.on_event(event);
    }) as Box<dyn FnMut(MessageEvent)>);
    channel.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
    std::mem::forget(channel);
}

fn route_to_item(queue_view: &Arc<Mutex<QueueView>>, event: &FfiPlayerEvent) {
    let FfiPlayerEvent::TrackStatusChanged { item_id, status } = event else {
        return;
    };
    let item = queue_view
        .lock()
        .iter()
        .find(|(id, _)| id == item_id)
        .map(|(_, item)| Arc::clone(item));
    let Some(item) = item else { return };
    update_item_state(&item, status);
    if let Some(item_obs) = item.observer() {
        dispatch_track_status_to_item(&item_obs, status);
    }
}

fn route_item_message(queue_view: &Arc<Mutex<QueueView>>, data: &JsValue) {
    let track_id = get_id_req(data, "track_id");
    let item_event = decode_item_event(data);
    let (Some(track_id), Some(item_event)) = (track_id, item_event) else {
        return;
    };
    let item = queue_view
        .lock()
        .iter()
        .find(|(id, _)| *id == track_id)
        .map(|(_, item)| Arc::clone(item));
    let Some(item) = item else { return };
    if let Some(obs) = item.observer() {
        obs.on_event(item_event);
    }
}

fn update_item_state(item: &Arc<AudioPlayerItem>, status: &FfiTrackStatus) {
    match status {
        FfiTrackStatus::Loaded => {
            let duration = item.duration_sec();
            item.state.lock().resolve_duration(duration);
        }
        FfiTrackStatus::Failed { .. } => {
            item.state.lock().mark_failed();
        }
        _ => {}
    }
}

fn dispatch_track_status_to_item(observer: &Arc<dyn ItemObserver>, status: &FfiTrackStatus) {
    match status {
        FfiTrackStatus::Loaded => observer.on_event(FfiItemEvent::StatusChanged {
            status: FfiItemStatus::ReadyToPlay,
        }),
        FfiTrackStatus::Failed { reason } => {
            observer.on_event(FfiItemEvent::StatusChanged {
                status: FfiItemStatus::Failed,
            });
            observer.on_event(FfiItemEvent::Error {
                error: reason.clone(),
            });
        }
        _ => {}
    }
}
