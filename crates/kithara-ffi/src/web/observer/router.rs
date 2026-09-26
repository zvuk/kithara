use std::mem;

use js_sys::{Function, Reflect};
use kithara::{
    events::TrackId,
    platform::sync::{Arc, Mutex},
};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use web_sys::{BroadcastChannel, MessageEvent, console};

use super::{decode::decode, decode_item::decode_item_event, marshal::get_id_req};
use crate::{
    item::AudioPlayerItem,
    observer::PlayerObserver,
    types::FfiPlayerEvent,
    web::{analysis::AnalysisRoute, observer::source::consts::EVENT_CHANNEL},
};

type QueueView = Vec<(TrackId, Arc<AudioPlayerItem>)>;

/// Main-thread fan-out of the worker event channel to the player, per-item and
/// analysis sinks.
#[derive(Clone)]
pub(crate) struct Routes {
    analysis: AnalysisRoute,
    queue_view: Arc<Mutex<QueueView>>,
    sinks: Arc<Mutex<Sinks>>,
}

#[derive(Default)]
struct Sinks {
    player: Option<Arc<dyn PlayerObserver>>,
    installed: bool,
}

impl Routes {
    pub(crate) fn new(queue_view: Arc<Mutex<QueueView>>) -> Self {
        Self {
            queue_view,
            sinks: Arc::new(Mutex::default()),
            analysis: AnalysisRoute::default(),
        }
    }

    fn arm(&self) {
        if self.sinks.lock().installed {
            return;
        }
        if self.install() {
            self.sinks.lock().installed = true;
        }
    }

    fn dispatch(&self, data: &JsValue) {
        let scope = scope(data);
        if self.analysis.dispatch(scope.as_deref(), data) {
            return;
        }
        match scope.as_deref() {
            Some("item") => self.route_item_message(data),
            _ => self.route_player(data),
        }
    }

    fn install(&self) -> bool {
        let Ok(channel) = BroadcastChannel::new(EVENT_CHANNEL) else {
            console::warn_1(&JsValue::from_str(
                "kithara: BroadcastChannel unavailable; observers disabled",
            ));
            return false;
        };
        let routes = self.clone();
        let closure = Closure::wrap(Box::new(move |ev: MessageEvent| {
            routes.dispatch(&ev.data());
        }) as Box<dyn FnMut(MessageEvent)>);
        channel.set_onmessage(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
        mem::forget(channel);
        true
    }

    fn item(&self, id: TrackId) -> Option<Arc<AudioPlayerItem>> {
        self.queue_view
            .lock()
            .iter()
            .find(|(existing, _)| *existing == id)
            .map(|(_, item)| Arc::clone(item))
    }

    fn route_item_message(&self, data: &JsValue) {
        let track_id = get_id_req(data, "track_id");
        let item_event = decode_item_event(data);
        let (Some(track_id), Some(item_event)) = (track_id, item_event) else {
            return;
        };
        let Some(item) = self.item(track_id) else {
            return;
        };
        item.deliver(item_event);
    }

    fn route_player(&self, data: &JsValue) {
        let Some(event) = decode(data) else {
            return;
        };
        self.route_to_item(&event);
        let observer = self.sinks.lock().player.clone();
        if let Some(observer) = observer {
            observer.on_event(event);
        }
    }

    fn route_to_item(&self, event: &FfiPlayerEvent) {
        let FfiPlayerEvent::TrackStatusChanged { item_id, status } = event else {
            return;
        };
        let Some(item) = self.item(*item_id) else {
            return;
        };
        item.apply_track_status(status);
    }

    pub(crate) fn set_analysis(&self, func: Function) {
        self.analysis.set(func);
        self.arm();
    }

    pub(crate) fn set_player(&self, observer: Arc<dyn PlayerObserver>) {
        self.sinks.lock().player = Some(observer);
        self.arm();
    }
}

fn scope(data: &JsValue) -> Option<String> {
    const SCOPE_KEY: &str = "scope";

    Reflect::get(data, &JsValue::from_str(SCOPE_KEY))
        .ok()
        .and_then(|value| value.as_string())
}
