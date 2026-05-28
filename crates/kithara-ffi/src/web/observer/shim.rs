use js_sys::{Function, Object, Reflect};
use send_wrapper::SendWrapper;
use wasm_bindgen::JsValue;

use crate::{
    observer::{ItemObserver, PlayerObserver, SeekCallback},
    types::{FfiItemEvent, FfiPlayerEvent, FfiTimeRange, FfiVariant},
    web::observer::source::encode as encode_player_event,
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

fn set_str(obj: &Object, key: &str, val: &str) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(val));
}

fn set_f64(obj: &Object, key: &str, val: f64) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_f64(val));
}

fn variant_to_js(variant: &FfiVariant) -> JsValue {
    let obj = Object::new();
    set_f64(&obj, "index", f64::from(variant.index));
    set_f64(
        &obj,
        "bandwidth_bps",
        num_traits::cast(variant.bandwidth_bps).unwrap_or(0.0),
    );
    if let Some(name) = variant.name.as_ref() {
        set_str(&obj, "name", name);
    }
    obj.into()
}

fn range_to_js(range: &FfiTimeRange) -> JsValue {
    let obj = Object::new();
    set_f64(&obj, "start_seconds", range.start_seconds);
    set_f64(&obj, "duration_seconds", range.duration_seconds);
    obj.into()
}

/// Marshal an [`FfiItemEvent`] into a plain JS object handed to the
/// per-item JS observer callback.
fn encode_item_event(event: &FfiItemEvent) -> JsValue {
    let obj = Object::new();
    match event {
        FfiItemEvent::DurationChanged { seconds } => {
            set_str(&obj, "kind", "DurationChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiItemEvent::LoadedRangesChanged { ranges } => {
            set_str(&obj, "kind", "LoadedRangesChanged");
            let arr = js_sys::Array::new();
            for r in ranges {
                arr.push(&range_to_js(r));
            }
            let _ = Reflect::set(&obj, &JsValue::from_str("ranges"), &arr);
        }
        FfiItemEvent::StatusChanged { status } => {
            set_str(&obj, "kind", "StatusChanged");
            set_f64(&obj, "status", item_status_code(*status));
        }
        FfiItemEvent::VariantsDiscovered { variants } => {
            set_str(&obj, "kind", "VariantsDiscovered");
            let arr = js_sys::Array::new();
            for v in variants {
                arr.push(&variant_to_js(v));
            }
            let _ = Reflect::set(&obj, &JsValue::from_str("variants"), &arr);
        }
        FfiItemEvent::VariantSelected { variant } => {
            set_str(&obj, "kind", "VariantSelected");
            let _ = Reflect::set(&obj, &JsValue::from_str("variant"), &variant_to_js(variant));
        }
        FfiItemEvent::VariantApplied { variant } => {
            set_str(&obj, "kind", "VariantApplied");
            let _ = Reflect::set(&obj, &JsValue::from_str("variant"), &variant_to_js(variant));
        }
        FfiItemEvent::DidReachEnd => set_str(&obj, "kind", "DidReachEnd"),
        FfiItemEvent::DidFail => set_str(&obj, "kind", "DidFail"),
        FfiItemEvent::DidStall => set_str(&obj, "kind", "DidStall"),
        FfiItemEvent::Error { error } => {
            set_str(&obj, "kind", "Error");
            set_str(&obj, "error", error);
        }
    }
    obj.into()
}

fn item_status_code(status: crate::types::FfiItemStatus) -> f64 {
    match status {
        crate::types::FfiItemStatus::Unknown => 0.0,
        crate::types::FfiItemStatus::ReadyToPlay => 1.0,
        crate::types::FfiItemStatus::Failed => 2.0,
    }
}
