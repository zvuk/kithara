use js_sys::{Function, Uint8Array};
use send_wrapper::SendWrapper;
use wasm_bindgen::{JsCast, JsValue};

use crate::{
    observer::{FfiKeyProcessor, ItemObserver, PlayerObserver, SeekCallback},
    types::{FfiItemEvent, FfiPlayerEvent},
    web::observer::{encode::encode as encode_player_event, encode_item::encode_item_event},
};

/// Bridges a JS callback into the [`PlayerObserver`] trait. The wrapped
/// [`Function`] is `!Send`; [`SendWrapper`] grants `Send + Sync` with a
/// runtime same-thread assertion, so `Arc<dyn PlayerObserver>` keeps the
/// trait's real `Send + Sync` supertraits on wasm without cfg.
pub(crate) struct PlayerObserverJs {
    func: SendWrapper<Function>,
}

impl PlayerObserverJs {
    pub(crate) fn new(func: Function) -> Self {
        Self {
            func: SendWrapper::new(func),
        }
    }
}

impl PlayerObserver for PlayerObserverJs {
    fn on_event(&self, event: FfiPlayerEvent) {
        let _ = self
            .func
            .call1(&JsValue::UNDEFINED, &encode_player_event(&event));
    }
}

/// Bridges a JS callback into the [`ItemObserver`] trait.
pub(crate) struct ItemObserverJs {
    func: SendWrapper<Function>,
}

impl ItemObserverJs {
    pub(crate) fn new(func: Function) -> Self {
        Self {
            func: SendWrapper::new(func),
        }
    }
}

impl ItemObserver for ItemObserverJs {
    fn on_event(&self, event: FfiItemEvent) {
        let _ = self
            .func
            .call1(&JsValue::UNDEFINED, &encode_item_event(&event));
    }
}

/// Bridges a JS callback into the [`SeekCallback`] trait.
pub(crate) struct SeekCallbackJs {
    func: SendWrapper<Function>,
}

impl SeekCallbackJs {
    pub(crate) fn new(func: Function) -> Self {
        Self {
            func: SendWrapper::new(func),
        }
    }
}

impl SeekCallback for SeekCallbackJs {
    fn on_complete(&self, finished: bool) {
        let _ = self
            .func
            .call1(&JsValue::UNDEFINED, &JsValue::from_bool(finished));
    }
}

/// Bridges a JS callback into the [`FfiKeyProcessor`] trait. The JS side
/// is `process_key(key: Uint8Array, salt: string) -> Uint8Array`, invoked
/// synchronously on the main thread by the
/// [`key_processor_bridge`](crate::web::key_processor_bridge) pump. A
/// non-`Uint8Array` return (or a throwing callback) yields an empty key,
/// so the decrypt fails loudly downstream rather than silently using the
/// raw bytes.
pub(crate) struct KeyProcessorJs {
    func: SendWrapper<Function>,
}

impl KeyProcessorJs {
    pub(crate) fn new(func: Function) -> Self {
        Self {
            func: SendWrapper::new(func),
        }
    }
}

impl FfiKeyProcessor for KeyProcessorJs {
    fn process_key(&self, key: Vec<u8>, salt: String) -> Vec<u8> {
        let key_js = Uint8Array::from(key.as_slice());
        let result = self.func.call2(
            &JsValue::UNDEFINED,
            key_js.as_ref(),
            &JsValue::from_str(&salt),
        );
        result
            .and_then(JsCast::dyn_into::<Uint8Array>)
            .map_or_else(|_| Vec::new(), |arr| arr.to_vec())
    }
}
