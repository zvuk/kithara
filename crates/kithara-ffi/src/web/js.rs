use std::sync::atomic::{AtomicU32, Ordering};

use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use num_traits::ToPrimitive;
use wasm_bindgen::prelude::*;
use web_sys::{BroadcastChannel, MessageEvent, console};

fn set_str(obj: &Object, key: &str, val: &str) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(val));
}

fn set_f64(obj: &Object, key: &str, val: f64) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_f64(val));
}

fn set_bool(obj: &Object, key: &str, val: bool) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_bool(val));
}

fn get_str(val: &JsValue, key: &str) -> Option<String> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
}

fn get_f64(val: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_f64())
}

fn get_bool(val: &JsValue, key: &str) -> Option<bool> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_bool())
}

pub(crate) fn next_request_id() -> u32 {
    static NEXT_REQUEST_ID: AtomicU32 = AtomicU32::new(1);
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// Length of the alphanumeric DRM salt. Mirrors `SALT_LEN` in
/// [`NativeInner`](crate::native::inner::NativeInner) and `kithara_app::drm`.
const SALT_LEN: usize = 16;

/// Generate a 16-character alphanumeric DRM salt on the main thread,
/// mirroring [`NativeInner`](crate::native::inner::NativeInner)'s
/// `generate_salt`. Sourced from the Web Crypto API
/// (`crypto.getRandomValues`) reached via global reflection so no extra
/// `web-sys` feature is required; falls back to the request-id counter if
/// the global `crypto` object is unavailable (non-secure context).
pub(crate) fn generate_salt() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

    let mut bytes = vec![0u8; SALT_LEN];
    if !fill_random(&mut bytes) {
        let seed = next_request_id();
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u32) as u8;
        }
    }
    bytes
        .into_iter()
        .map(|b| ALPHABET[(b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// Fill `out` with cryptographically strong random bytes via the global
/// `crypto.getRandomValues`. Returns `false` if `crypto` is unreachable.
fn fill_random(out: &mut [u8]) -> bool {
    let global = js_sys::global();
    let Ok(crypto) = Reflect::get(&global, &JsValue::from_str("crypto")) else {
        return false;
    };
    let Ok(get_random) = Reflect::get(&crypto, &JsValue::from_str("getRandomValues")) else {
        return false;
    };
    let Ok(func) = get_random.dyn_into::<Function>() else {
        return false;
    };
    let view = Uint8Array::new_with_length(out.len() as u32);
    if func.call1(&crypto, view.as_ref()).is_err() {
        return false;
    }
    view.copy_to(out);
    true
}

/// Send a one-shot reply from Worker to main thread.
pub(crate) fn send_reply(request_id: u32, result: Result<(), String>) {
    let Ok(bc) = BroadcastChannel::new("kithara-reply") else {
        return;
    };
    let obj = Object::new();
    set_f64(&obj, "request_id", f64::from(request_id));
    match result {
        Ok(()) => set_bool(&obj, "ok", true),
        Err(e) => {
            set_bool(&obj, "ok", false);
            set_str(&obj, "error", &e);
        }
    }
    let _ = bc.post_message(&obj.into());
    bc.close();
}

/// Create a JS Promise that resolves with `undefined` when the Worker
/// sends an `ok` reply with matching `request_id` via
/// `BroadcastChannel("kithara-reply")`, or rejects with the error string.
pub(crate) fn reply_promise(request_id: u32) -> Result<Promise, JsValue> {
    reply_promise_with_value(request_id, JsValue::UNDEFINED)
}

/// Like [`reply_promise`] but resolves with `value` instead of
/// `undefined`. Used by queue ops that hand a freshly-allocated track id
/// back to the caller once the worker confirms placement.
pub(crate) fn reply_promise_with_value(
    request_id: u32,
    value: impl Into<JsValue>,
) -> Result<Promise, JsValue> {
    let bc = BroadcastChannel::new("kithara-reply")?;
    let value: JsValue = value.into();

    let promise = Promise::new(&mut |resolve, reject| {
        let bc_ref = bc.clone();
        let value = value.clone();
        let closure = Closure::wrap(Box::new(move |ev: MessageEvent| {
            let data = ev.data();
            let rid = get_f64(&data, "request_id")
                .and_then(|v| v.to_u32())
                .unwrap_or(0);
            if rid != request_id {
                return;
            }
            bc_ref.set_onmessage(None);
            bc_ref.close();
            if get_bool(&data, "ok").unwrap_or(false) {
                let _ = resolve.call1(&JsValue::UNDEFINED, &value);
            } else {
                let err = get_str(&data, "error").unwrap_or_else(|| "unknown error".into());
                let _ = reject.call1(&JsValue::UNDEFINED, &JsValue::from_str(&err));
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        bc.set_onmessage(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
    });

    Ok(promise)
}
/// Init event log receiver on the main thread.
pub(crate) fn init_event_reader() {
    /// Maximum number of events in the event log ring buffer.
    const MAX_EVENTS: u32 = 1024;

    let global = js_sys::global();
    let init_key = JsValue::from_str("__kithara_event_reader_init");
    if Reflect::get(&global, &init_key)
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return;
    }
    let _ = Reflect::set(&global, &init_key, &JsValue::from_bool(true));

    let queue = Array::new();
    let _ = Reflect::set(&global, &JsValue::from_str("__kithara_event_queue"), &queue);

    let Ok(bc) = BroadcastChannel::new("kithara-events") else {
        console::warn_1(&JsValue::from_str(
            "kithara: BroadcastChannel unavailable; event reader disabled",
        ));
        return;
    };
    let q = queue.clone();
    let closure = Closure::wrap(Box::new(move |ev: MessageEvent| {
        q.push(&ev.data());
        while q.length() > MAX_EVENTS {
            q.shift();
        }
    }) as Box<dyn FnMut(_)>);
    bc.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
    std::mem::forget(bc);
}

/// Read and drain all accumulated events from the JS-side queue.
pub(crate) fn take_events() -> String {
    let global = js_sys::global();
    let Ok(val) = Reflect::get(&global, &JsValue::from_str("__kithara_event_queue")) else {
        return String::new();
    };
    if val.is_undefined() || val.is_null() {
        return String::new();
    }
    let queue: Array = val.unchecked_into();
    let len = queue.length();
    if len == 0 {
        return String::new();
    }
    let mut lines = Vec::with_capacity(len as usize);
    for _ in 0..len {
        if let Some(s) = queue.shift().as_string() {
            lines.push(s);
        }
    }
    lines.join("\n")
}
