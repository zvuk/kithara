use std::{collections::HashSet, mem, sync::OnceLock};

use js_sys::{Function, Reflect};
use kithara::{
    events::TrackId,
    platform::sync::{Arc, Mutex},
};
use kithara_command::Seq;
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use web_sys::{BroadcastChannel, MessageEvent, console};

use super::{decode::decode, decode_item::decode_item_event, marshal::get_id_req};
use crate::{
    item::AudioPlayerItem,
    observer::PlayerObserver,
    pools::FfiQueueControl,
    types::FfiPlayerEvent,
    web::{analysis::AnalysisRoute, inner::ItemRegistry, observer::source::consts::EVENT_CHANNEL},
};

/// Main-thread fan-out of the worker event channel to the player, per-item and
/// analysis sinks.
#[derive(Clone)]
pub(crate) struct Routes {
    analysis: AnalysisRoute,
    items: Arc<Mutex<ItemRegistry>>,
    queue: Arc<OnceLock<FfiQueueControl>>,
    sinks: Arc<Mutex<Sinks>>,
}

#[derive(Default)]
struct Sinks {
    player: Option<Arc<dyn PlayerObserver>>,
    installed: bool,
}

impl Routes {
    pub(in crate::web) fn new(
        items: Arc<Mutex<ItemRegistry>>,
        queue: Arc<OnceLock<FfiQueueControl>>,
    ) -> Self {
        Self {
            items,
            queue,
            sinks: Arc::new(Mutex::default()),
            analysis: AnalysisRoute::default(),
        }
    }

    pub(in crate::web) fn arm(&self) -> bool {
        if self.sinks.lock().installed {
            return true;
        }
        let installed = self.install();
        self.sinks.lock().installed = installed;
        installed
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
        self.items.lock().get(id)
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
        if matches!(
            event,
            FfiPlayerEvent::TrackAdded { .. } | FfiPlayerEvent::TrackRemoved { .. }
        ) {
            self.reconcile();
        }
        self.route_to_item(&event);
        let observer = self.sinks.lock().player.clone();
        if let Some(observer) = observer {
            observer.on_event(event);
        }
    }

    pub(in crate::web) fn complete(&self, seq: Seq) {
        let Some(queue) = self.queue.get() else {
            return;
        };
        let members: HashSet<_> = queue.tracks().into_iter().map(|track| track.id).collect();
        let (prime, answers) = self.items.lock().complete(seq, &members);
        for item in prime {
            item.restart_bridge();
        }
        for sender in answers {
            if sender.send(()).is_err() {
                tracing::trace!("analysis admission observer was dropped");
            }
        }
    }

    pub(in crate::web) fn error(&self, error: String) {
        let observer = self.sinks.lock().player.clone();
        if let Some(observer) = observer {
            observer.on_event(FfiPlayerEvent::Error { error });
        } else {
            tracing::warn!(%error, "Web player command refused");
        }
    }

    fn reconcile(&self) {
        let Some(queue) = self.queue.get() else {
            return;
        };
        let members: HashSet<_> = queue.tracks().into_iter().map(|track| track.id).collect();
        let prime = self.items.lock().reconcile(&members);
        for item in prime {
            item.restart_bridge();
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
