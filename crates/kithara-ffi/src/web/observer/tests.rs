#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect, Uint8Array};
use kithara::events::TrackId;
use kithara_ffi::player::AudioPlayer;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

mod decode;
mod decode_item;
mod encode;
mod encode_item;
mod marshal;

mod types {
    pub(crate) use kithara_ffi::types::*;
}

use types::{
    FfiDecodeErrorKind, FfiItemEvent, FfiKeySource, FfiPlayerEvent, FfiStretchBackendKind,
    FfiTrackFailureKind, FfiTrackStatus,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_SAFE_INTEGER_F64: f64 = 9_007_199_254_740_991.0;

fn surface_ids_fixture() -> (AudioPlayer, String, f64) {
    let wav = Uint8Array::from(
        &b"RIFF\x26\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0\x77\x01\0\x02\0\x10\0data\x02\0\0\0\0\0"[..],
    );
    let blob = web_sys::Blob::new_with_u8_array_sequence(&Array::of1(&wav)).expect("WAV blob");
    let url_type = Reflect::get(&js_sys::global(), &JsValue::from_str("URL")).expect("URL type");
    let create_url = Reflect::get(&url_type, &JsValue::from_str("createObjectURL"))
        .expect("createObjectURL")
        .dyn_into::<Function>()
        .expect("createObjectURL function");
    let url = create_url
        .call1(&url_type, &blob)
        .expect("create WAV URL")
        .as_string()
        .expect("WAV URL string");
    let player = AudioPlayer::new_js();
    let id = player.insert_js(url.clone(), -1.0).expect("insert fixture");
    assert_eq!(player.item_count_js(), 1);
    (player, url, id)
}

#[wasm_bindgen_test]
fn surface_ids_fractional_track_id_cannot_remove_known_item() {
    let (player, _, id) = surface_ids_fixture();

    assert!(player.remove_js(id + 0.5).is_err());
    assert_eq!(player.item_count_js(), 1);
    player.remove_js(id).expect("remove exact fixture id");
    assert_eq!(player.item_count_js(), 0);
}

#[wasm_bindgen_test]
fn surface_ids_insert_requires_known_anchor_and_preserves_negative_head() {
    let (player, url, first) = surface_ids_fixture();

    assert!(player.insert_js(url.clone(), first + 1.0).is_err());
    assert_eq!(player.item_count_js(), 1);
    let tail = player.insert_js(url.clone(), first).expect("known anchor");
    let head = player.insert_js(url, -2.0).expect("negative head anchor");
    let ids: Vec<f64> = player
        .items()
        .iter()
        .map(|item| num_traits::cast(item.queue_id().as_u64()).expect("safe fixture id"))
        .collect();
    assert_eq!(ids, [head, first, tail]);
}

#[wasm_bindgen_test]
fn surface_ids_malformed_insert_anchor_leaves_queue_unchanged() {
    let (player, url, id) = surface_ids_fixture();

    for raw in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        id + 0.5,
        -0.5,
        MAX_SAFE_INTEGER_F64 + 1.0,
        f64::MAX,
        -f64::MAX,
    ] {
        assert!(player.insert_js(url.clone(), raw).is_err(), "anchor {raw}");
        assert_eq!(player.item_count_js(), 1, "anchor {raw}");
    }
}

#[wasm_bindgen_test]
fn surface_ids_malformed_observer_id_is_rejected() {
    let (player, _, id) = surface_ids_fixture();
    let callback = Function::new_no_args("");
    let observer = player
        .add_item_observer_js(id, callback.into())
        .expect("add item observer");

    for raw in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        observer + 0.5,
        -1.0,
        MAX_SAFE_INTEGER_F64 + 1.0,
    ] {
        assert!(
            player.remove_item_observer_js(id, raw).is_err(),
            "observer {raw}"
        );
    }
    player
        .remove_item_observer_js(id, observer)
        .expect("remove exact observer id");
}

fn keys(value: &JsValue) -> Vec<String> {
    let mut keys = Reflect::own_keys(value)
        .expect("wire keys")
        .iter()
        .map(|key| key.as_string().expect("string key"))
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

#[wasm_bindgen_test]
fn stretch_backend_event_preserves_kind_and_backend() {
    let encoded = encode::encode(&FfiPlayerEvent::DjStretchBackendChanged {
        kind: FfiStretchBackendKind::Bungee,
    });

    let kind = Reflect::get(&encoded, &JsValue::from_str("kind"))
        .expect("event kind")
        .as_string();
    let backend = Reflect::get(&encoded, &JsValue::from_str("backend"))
        .expect("backend payload")
        .as_string();

    assert_eq!(kind.as_deref(), Some("DjStretchBackendChanged"));
    assert_eq!(backend.as_deref(), Some("Bungee"));
    assert!(matches!(
        decode::decode(&encoded),
        Some(FfiPlayerEvent::DjStretchBackendChanged {
            kind: FfiStretchBackendKind::Bungee
        })
    ));
}

#[wasm_bindgen_test]
fn track_status_schema_preserves_max_safe_item_id() {
    let encoded = encode::encode(&FfiPlayerEvent::TrackStatusChanged {
        item_id: TrackId(MAX_SAFE_INTEGER),
        status: FfiTrackStatus::Loaded,
    });

    assert_eq!(keys(&encoded), ["item_id", "kind", "status"]);
    assert_eq!(
        marshal::get_str(&encoded, "kind").as_deref(),
        Some("TrackStatusChanged")
    );
    assert_eq!(
        marshal::get_f64(&encoded, "item_id"),
        Some(MAX_SAFE_INTEGER_F64)
    );
    assert_eq!(marshal::get_f64(&encoded, "status"), Some(3.0));
    assert!(!Reflect::has(&encoded, &JsValue::from_str("reason")).expect("reason presence"));
    assert!(matches!(
        decode::decode(&encoded),
        Some(FfiPlayerEvent::TrackStatusChanged {
            item_id: TrackId(MAX_SAFE_INTEGER),
            status: FfiTrackStatus::Loaded
        })
    ));
}

#[wasm_bindgen_test]
fn drm_key_schema_omits_absent_optional_fields() {
    let encoded = encode_item::encode_item_event(&FfiItemEvent::DrmKeyAcquired {
        key_host: None,
        source: FfiKeySource::DiskCache,
        bytes: MAX_SAFE_INTEGER,
        latency_ms: None,
    });

    assert_eq!(keys(&encoded), ["bytes", "kind", "source"]);
    assert_eq!(
        marshal::get_str(&encoded, "kind").as_deref(),
        Some("DrmKeyAcquired")
    );
    assert_eq!(
        marshal::get_str(&encoded, "source").as_deref(),
        Some("DiskCache")
    );
    assert_eq!(
        marshal::get_f64(&encoded, "bytes"),
        Some(MAX_SAFE_INTEGER_F64)
    );
    assert!(!Reflect::has(&encoded, &JsValue::from_str("key_host")).expect("key host presence"));
    assert!(!Reflect::has(&encoded, &JsValue::from_str("latency_ms")).expect("latency presence"));
    assert!(matches!(
        decode_item::decode_item_event(&encoded),
        Some(FfiItemEvent::DrmKeyAcquired {
            key_host: None,
            source: FfiKeySource::DiskCache,
            bytes: MAX_SAFE_INTEGER,
            latency_ms: None
        })
    ));
}

#[wasm_bindgen_test]
fn track_failure_preserves_kind_and_offset_without_epoch() {
    let cases = [
        (
            FfiTrackFailureKind::Decode {
                kind: FfiDecodeErrorKind::Io,
            },
            "Decode",
            Some("Io"),
            None,
        ),
        (
            FfiTrackFailureKind::Decode {
                kind: FfiDecodeErrorKind::InvalidData,
            },
            "Decode",
            Some("InvalidData"),
            None,
        ),
        (
            FfiTrackFailureKind::RecreateFailed {
                offset: MAX_SAFE_INTEGER,
            },
            "RecreateFailed",
            None,
            Some(MAX_SAFE_INTEGER_F64),
        ),
        (
            FfiTrackFailureKind::SourceCancelled,
            "SourceCancelled",
            None,
            None,
        ),
        (
            FfiTrackFailureKind::ChannelClosed,
            "ChannelClosed",
            None,
            None,
        ),
        (FfiTrackFailureKind::Render, "Render", None, None),
    ];

    for (reason, reason_name, decode_kind, offset) in cases {
        let encoded = encode_item::encode_item_event(&FfiItemEvent::TrackFailed {
            reason: reason.clone(),
        });

        assert_eq!(
            marshal::get_str(&encoded, "kind").as_deref(),
            Some("TrackFailed")
        );
        assert_eq!(
            marshal::get_str(&encoded, "reason").as_deref(),
            Some(reason_name)
        );
        assert_eq!(
            marshal::get_str(&encoded, "decode_kind").as_deref(),
            decode_kind
        );
        assert_eq!(marshal::get_f64(&encoded, "offset"), offset);
        assert!(!Reflect::has(&encoded, &JsValue::from_str("epoch")).expect("epoch presence"));
        assert!(matches!(
            decode_item::decode_item_event(&encoded),
            Some(FfiItemEvent::TrackFailed {
                reason: decoded_reason,
            }) if decoded_reason == reason
        ));
    }
}
