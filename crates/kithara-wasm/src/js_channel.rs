//! JS-heap channels for reply and event transport.
//!
//! Command payloads use shared-memory MPSC from `kithara_platform::sync::mpsc`.
//! Reply and event signals use BroadcastChannel in the JS heap.

use std::sync::atomic::{AtomicU32, Ordering};

use js_sys::{Array, Object, Promise, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{BroadcastChannel, MessageEvent};

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

static NEXT_REQUEST_ID: AtomicU32 = AtomicU32::new(1);

pub(crate) fn next_request_id() -> u32 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
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

/// Create a JS Promise that resolves when the Worker sends a reply with
/// matching `request_id` via `BroadcastChannel("kithara-reply")`.
pub(crate) fn reply_promise(request_id: u32) -> Result<Promise, JsValue> {
    let bc = BroadcastChannel::new("kithara-reply")?;

    let promise = Promise::new(&mut |resolve, reject| {
        let bc_ref = bc.clone();
        let closure = Closure::wrap(Box::new(move |ev: MessageEvent| {
            let data = ev.data();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let rid = get_f64(&data, "request_id").unwrap_or(0.0) as u32;
            if rid != request_id {
                return;
            }
            bc_ref.set_onmessage(None);
            bc_ref.close();
            if get_bool(&data, "ok").unwrap_or(false) {
                let _ = resolve.call0(&JsValue::UNDEFINED);
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
/// Maximum number of events in the event log ring buffer.
const MAX_EVENTS: u32 = 1024;

/// Init event log receiver on the main thread.
pub(crate) fn init_event_reader() {
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

    let bc = BroadcastChannel::new("kithara-events").expect("BroadcastChannel not supported");
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
