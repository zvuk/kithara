use std::sync::atomic::{AtomicU32, Ordering};

use js_sys::{Function, Object, Reflect, Uint8Array};
use num_traits::cast;
use wasm_bindgen::prelude::*;
use web_sys::BroadcastChannel;

fn set_str(obj: &Object, key: &str, val: &str) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(val));
}

fn set_f64(obj: &Object, key: &str, val: f64) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_f64(val));
}

fn set_bool(obj: &Object, key: &str, val: bool) {
    let _ = Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_bool(val));
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
            let offset: u32 = cast(i).unwrap_or(0);
            let mixed = seed.wrapping_add(offset);
            *b = cast::<u32, u8>(mixed & u32::from(u8::MAX)).unwrap_or(0);
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
    let Some(len): Option<u32> = cast(out.len()) else {
        return false;
    };
    let view = Uint8Array::new_with_length(len);
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
