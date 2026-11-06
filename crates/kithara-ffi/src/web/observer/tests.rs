#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect, Uint8Array};
use kithara::{
    events::TrackId,
    platform::{
        sync::{Arc, Mutex},
        time::Duration,
    },
    play::PlayError,
    queue::QueueError,
};
use kithara_ffi::{
    item::AudioPlayerItem,
    observer::{PlayerObserver, SeekCallback},
    player::AudioPlayer,
};
use kithara_test_utils::wait_until;
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
    FfiDecodeErrorKind, FfiItemConfig, FfiItemEvent, FfiKeySource, FfiPlayerEvent,
    FfiStretchBackendKind, FfiTrackFailureKind, FfiTrackStatus,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_SAFE_INTEGER_F64: f64 = 9_007_199_254_740_991.0;

fn surface_fixture_url() -> String {
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
    url
}

async fn surface_ids_fixture() -> (AudioPlayer, String, f64) {
    let url = surface_fixture_url();
    let player = AudioPlayer::new_js();
    let id = player.insert_js(url.clone(), -1.0).expect("insert fixture");
    wait_until(Duration::from_secs(5), "fixture insertion", || {
        player.tick_js();
        player.item_count_js() == 1
    })
    .await
    .expect("owner admits fixture insertion");
    (player, url, id)
}

#[wasm_bindgen_test]
async fn surface_ids_fractional_track_id_cannot_remove_known_item() {
    let (player, _, id) = surface_ids_fixture().await;

    assert!(player.remove_js(id + 0.5).is_err());
    assert_eq!(player.item_count_js(), 1);
    player.remove_js(id).expect("remove exact fixture id");
    assert_eq!(player.item_count_js(), 1, "removal needs the owner verdict");
    wait_until(Duration::from_secs(5), "fixture removal", || {
        player.tick_js();
        player.item_count_js() == 0
    })
    .await
    .expect("owner removes exact fixture id");
    assert_eq!(player.item_count_js(), 0);
}

#[wasm_bindgen_test]
async fn surface_ids_insert_requires_known_anchor_and_preserves_negative_head() {
    let (player, url, first) = surface_ids_fixture().await;

    assert!(player.insert_js(url.clone(), first + 1.0).is_err());
    assert_eq!(player.item_count_js(), 1);
    let tail = player.insert_js(url.clone(), first).expect("known anchor");
    wait_until(Duration::from_secs(5), "tail insertion", || {
        player.tick_js();
        player.item_count_js() == 2
    })
    .await
    .expect("owner admits tail insertion");
    let head = player.insert_js(url, -2.0).expect("negative head anchor");
    wait_until(Duration::from_secs(5), "head insertion", || {
        player.tick_js();
        player.item_count_js() == 3
    })
    .await
    .expect("owner admits head insertion");
    let ids: Vec<f64> = player
        .items()
        .iter()
        .map(|item| num_traits::cast(item.queue_id().as_u64()).expect("safe fixture id"))
        .collect();
    assert_eq!(ids, [head, first, tail]);
}

#[wasm_bindgen_test]
async fn surface_ids_malformed_insert_anchor_leaves_queue_unchanged() {
    let (player, url, id) = surface_ids_fixture().await;

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
async fn surface_ids_malformed_observer_id_is_rejected() {
    let (player, _, id) = surface_ids_fixture().await;
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

#[wasm_bindgen_test]
async fn surface_dependent_commands_share_owner_order_before_publication() {
    let player = Arc::new(AudioPlayer::new_js());
    let url = surface_fixture_url();
    let events = Arc::new(Mutex::new(Vec::new()));
    player.set_observer(Arc::new(Events(Arc::clone(&events))));
    player
        .set_action_at_item_end_js("none".to_owned())
        .expect("queue end policy");
    let first = player.append_js(url.clone()).expect("post append");
    player
        .select_js(first)
        .expect("select the earlier pending append");
    let tail = player
        .insert_js(url.clone(), first)
        .expect("insert after the earlier pending append");
    let disposable = player.append_js(url).expect("post disposable append");
    player
        .remove_js(disposable)
        .expect("remove the earlier pending append");
    let observer = player
        .add_item_observer_js(first, Function::new_no_args("").into())
        .expect("observe the retained pending caller object");
    player
        .remove_item_observer_js(first, observer)
        .expect("remove pending item observer");
    player
        .analyze_js(first)
        .expect("analysis waits for the earlier append admission");
    assert_eq!(
        player.item_count_js(),
        0,
        "commands do not publish membership"
    );
    assert!(
        player.items().is_empty(),
        "getters use only the owner snapshot"
    );
    assert!(player.remove_js(first + 0.5).is_err());
    assert!(player.select_js(first + 0.5).is_err());
    assert!(player.select_js(MAX_SAFE_INTEGER_F64).is_err());
    assert!(
        player
            .insert_js(surface_fixture_url(), MAX_SAFE_INTEGER_F64)
            .is_err()
    );
    wait_until(Duration::from_secs(5), "dependent owner commands", || {
        player.tick_js();
        player.item_count_js() == 2
            && events.lock().iter().any(|event| {
                matches!(event, FfiPlayerEvent::TrackRemoved { item_id }
                if item_id.as_u64() == num_traits::cast::<f64, u64>(disposable).expect("safe id"))
            })
    })
    .await
    .expect("the owner applies dependent commands in posted order");
    let ids: Vec<f64> = player
        .items()
        .iter()
        .map(|item| num_traits::cast(item.queue_id().as_u64()).expect("safe id"))
        .collect();
    assert_eq!(ids, [first, tail]);
    assert!(
        events
            .lock()
            .iter()
            .all(|event| !matches!(event, FfiPlayerEvent::Error { .. })),
        "known pending command targets must reach owner validation in order"
    );
}

fn caller_item(url: &str) -> Arc<AudioPlayerItem> {
    AudioPlayerItem::new(FfiItemConfig {
        url: url.to_owned(),
        abr_mode: None,
        headers: None,
        audio_id: None,
        uuid_i64: None,
        is_live_stream: false,
        preferred_peak_bitrate: 0.0,
        preferred_peak_bitrate_expensive: 0.0,
    })
}

struct Events(Arc<Mutex<Vec<FfiPlayerEvent>>>);

impl PlayerObserver for Events {
    fn on_event(&self, event: FfiPlayerEvent) {
        self.0.lock().push(event);
    }
}

struct Seeks(Arc<Mutex<Vec<bool>>>);

impl SeekCallback for Seeks {
    fn on_complete(&self, finished: bool) {
        self.0.lock().push(finished);
    }
}

#[wasm_bindgen_test]
async fn surface_settings_follow_owner_publication_and_report_refusal() {
    let player = Arc::new(AudioPlayer::new_js());
    let events = Arc::new(Mutex::new(Vec::new()));
    player.set_observer(Arc::new(Events(Arc::clone(&events))));
    let original_rate = player.playing_rate();
    let original_volume = player.volume();
    player
        .set_playing_rate_js(0.0)
        .expect("admit speed command");
    player.set_volume_js(0.25).expect("admit volume command");
    player.set_muted_js(true).expect("admit mute command");
    assert_eq!(
        player.playing_rate(),
        original_rate,
        "enqueue must not publish speed"
    );
    assert_eq!(
        player.volume(),
        original_volume,
        "enqueue must not publish volume"
    );
    assert!(!player.is_muted(), "enqueue must not publish mute");
    let document = web_sys::window()
        .expect("browser window")
        .document()
        .expect("browser document");
    wait_until(Duration::from_secs(5), "owner settings", || {
        player.tick_js();
        // The backend installs its resume listener after the worklet starts.
        let click = web_sys::Event::new("click").expect("build a click event");
        document.dispatch_event(&click).expect("dispatch the click");
        player.playing_rate() == kithara::play::MIN_SPEED
            && player.volume() == 0.25
            && player.is_muted()
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "{error}; rate={}, volume={}, muted={}, process_calls={}, events={:?}",
            player.playing_rate(),
            player.volume(),
            player.is_muted(),
            player.rt_process_calls_js(),
            *events.lock(),
        )
    });
    player
        .set_volume_js(0.75)
        .expect("admit volume while muted");
    wait_until(Duration::from_secs(5), "muted volume", || {
        player.tick_js();
        player.volume() == 0.75 && player.is_muted()
    })
    .await
    .expect("the owner keeps volume independent of mute");
    player.set_muted_js(false).expect("admit unmute");
    wait_until(Duration::from_secs(5), "unmuted volume", || {
        player.tick_js();
        player.volume() == 0.75 && !player.is_muted()
    })
    .await
    .expect("unmute keeps the confirmed volume");
    assert!(player.set_playing_rate_js(f32::NAN).is_err());
    assert_eq!(player.playing_rate(), kithara::play::MIN_SPEED);
    let bands = player.eq_band_count();
    let expected = PlayError::EqBandOutOfRange {
        band: bands as usize,
        bands: bands as usize,
    }
    .to_string();
    player
        .set_eq_gain_js(bands, 3.0)
        .expect("admit command before the owner checks the band");
    wait_until(Duration::from_secs(5), "EQ owner refusal", || {
        player.tick_js();
        events.lock().iter().any(|event| {
            matches!(event,
            FfiPlayerEvent::Error { error } if error == &expected)
        })
    })
    .await
    .expect("the original owner error reaches the public observer");
    assert_eq!(player.eq_band_count(), bands);
    assert_eq!(player.eq_gain(0), 0.0);
    let completions = Arc::new(Mutex::new(Vec::new()));
    player.seek(0.0, None, Arc::new(Seeks(Arc::clone(&completions))));
    assert!(
        completions.lock().is_empty(),
        "seek waits for the owner answer"
    );
    wait_until(Duration::from_secs(5), "seek owner answer", || {
        player.tick_js();
        *completions.lock() == [true]
    })
    .await
    .expect("owner completion reaches the main-thread callback");
    assert_eq!(player.snapshot().current_time, Some(0.0));
    assert_eq!(player.snapshot().duration, None);
}

#[wasm_bindgen_test]
async fn surface_rejected_insert_releases_its_object_and_keeps_owner_membership() {
    let (player, url, _) = surface_ids_fixture().await;
    let player = Arc::new(player);
    let anchor = player.items().pop().expect("confirmed anchor");
    let anchor_id = anchor.queue_id();
    let candidate = caller_item(&url);
    let candidate_ref = Arc::downgrade(&candidate);
    let events = Arc::new(Mutex::new(Vec::new()));
    player.set_observer(Arc::new(Events(Arc::clone(&events))));
    player.remove(&anchor).expect("admit anchor removal");
    player
        .insert(Arc::clone(&candidate), Some(anchor))
        .expect("anchor is still in the published snapshot");
    assert_eq!(
        player.item_count(),
        1,
        "queued work must not rewrite membership"
    );
    drop(candidate);
    assert!(
        candidate_ref.upgrade().is_some(),
        "pending admission retains its object"
    );
    let expected = QueueError::UnknownTrackId(anchor_id).to_string();
    wait_until(Duration::from_secs(5), "rejected insertion custody", || {
        player.tick_js();
        player.item_count() == 0
            && candidate_ref.upgrade().is_none()
            && events.lock().iter().any(|event| {
                matches!(event,
                FfiPlayerEvent::Error { error } if error == &expected)
            })
    })
    .await
    .expect("owner refusal releases the preregistered object");
    assert!(player.items().is_empty());
    assert!(player.current_item().is_none());
}

#[wasm_bindgen_test]
async fn surface_replacement_keeps_its_object_until_published_removal() {
    let (player, url, _) = surface_ids_fixture().await;
    let player = Arc::new(player);
    let item = caller_item(&url);
    let id = item.queue_id();
    let item_ref = Arc::downgrade(&item);
    player.replace_item(0, item).expect("admit replacement");
    wait_until(Duration::from_secs(5), "replacement publication", || {
        player.tick_js();
        let items = player.items();
        items.len() == 1 && items[0].queue_id() == id
    })
    .await
    .expect("replacement remains registered after owner admission");
    assert!(item_ref.upgrade().is_some());
    player.remove_all_items();
    wait_until(Duration::from_secs(5), "replacement object removal", || {
        player.tick_js();
        player.item_count() == 0 && item_ref.upgrade().is_none()
    })
    .await
    .expect("published removal releases the replacement object");
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
fn track_failure_preserves_kind_offset_and_epoch() {
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
