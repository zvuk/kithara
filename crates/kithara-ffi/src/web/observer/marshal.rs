use js_sys::{Object, Reflect};
use kithara_events::TrackId;
use wasm_bindgen::JsValue;

pub(crate) const KIND: &str = "kind";

pub(crate) fn set_str(obj: &Object, key: &str, val: &str) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(val));
}

pub(crate) fn set_f64(obj: &Object, key: &str, val: f64) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_f64(val));
}

pub(crate) fn set_bool(obj: &Object, key: &str, val: bool) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_bool(val));
}

#[rustfmt::skip]
pub(crate) fn set_opt_f64(obj: &Object, key: &str, val: Option<f64>) { if let Some(val) = val { set_f64(obj, key, val); } }

#[rustfmt::skip]
pub(crate) fn set_opt_str(obj: &Object, key: &str, val: Option<&str>) { if let Some(val) = val { set_str(obj, key, val); } }

#[rustfmt::skip]
pub(crate) fn set_opt_id(obj: &Object, key: &str, id: Option<TrackId>) { if let Some(id) = id { set_f64(obj, key, num_traits::cast(id.as_u64()).unwrap_or(0.0)); } }

pub(crate) fn get_str(val: &JsValue, key: &str) -> Option<String> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
}

pub(crate) fn get_f64(val: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_f64())
}

pub(crate) fn get_bool(val: &JsValue, key: &str) -> Option<bool> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_bool())
}

pub(crate) fn get_opt_id(val: &JsValue, key: &str) -> Option<TrackId> {
    get_f64(val, key).map(|raw| TrackId(num_traits::cast(raw.max(0.0)).unwrap_or(0)))
}

pub(crate) fn get_id_req(val: &JsValue, key: &str) -> Option<TrackId> {
    Some(TrackId(
        num_traits::cast(get_f64(val, key)?.max(0.0)).unwrap_or(0),
    ))
}

pub(crate) fn narrow_f32(value: f64) -> f32 {
    num_traits::cast(value).unwrap_or(0.0)
}

pub(crate) fn discriminant(code: f64) -> u32 {
    num_traits::cast(code).unwrap_or(0)
}
