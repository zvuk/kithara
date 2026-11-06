#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc, sync::Once};

use js_sys::{Array, Float32Array, Float64Array, Object, Reflect};
use kithara::{
    platform::{
        sync::Arc,
        time::{Duration, sleep},
    },
    queue::QueueError,
};
use kithara_ffi::{item::AudioPlayerItem, player::AudioPlayer, types::FfiItemConfig};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{BroadcastChannel, MessageEvent};

wasm_bindgen_test_configure!(run_in_browser);

const SERVER_URL: &str = "http://127.0.0.1:3444";
const CLICKS_PATH: &str = "/signal/signal_mp3_clicks126_30s.mp3";
const POLL_MS: u64 = 50;
const DEADLINE_MS: u64 = 30_000;
const RESTART_SAMPLE: usize = 3;

type Events = Rc<RefCell<Vec<JsValue>>>;

struct BroadcastObservation {
    channel: BroadcastChannel,
    _listener: Closure<dyn FnMut(MessageEvent)>,
    messages: Events,
}

impl BroadcastObservation {
    fn new(name: &str) -> Self {
        let channel = BroadcastChannel::new(name).expect("diagnostic channel");
        let messages: Events = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&messages);
        let listener = Closure::wrap(Box::new(move |event: MessageEvent| {
            let data = event.data();
            let summary = Object::new();
            let analysis = get(&data, "scope").as_string().as_deref() == Some("analysis");
            for key in keys(&data) {
                let value = get(&data, &key);
                // Keep analysis array lengths instead of retaining or printing their contents.
                let value =
                    if analysis && matches!(key.as_str(), "waveform" | "beats" | "downbeats") {
                        get(&value, "length")
                    } else {
                        value
                    };
                Reflect::set(&summary, &JsValue::from_str(&key), &value)
                    .expect("diagnostic message field");
            }
            sink.borrow_mut().push(summary.into());
        }) as Box<dyn FnMut(MessageEvent)>);
        channel.set_onmessage(Some(listener.as_ref().unchecked_ref()));
        Self {
            channel,
            _listener: listener,
            messages,
        }
    }

    fn report(&self) -> String {
        let messages = Array::new();
        for message in self.messages.borrow().iter() {
            messages.push(message);
        }
        js_sys::JSON::stringify(&messages)
            .expect("diagnostic messages serialize")
            .as_string()
            .expect("diagnostic report string")
    }
}

impl Drop for BroadcastObservation {
    fn drop(&mut self) {
        self.channel.set_onmessage(None);
        self.channel.close();
    }
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).expect("analysis field")
}

fn number(value: &JsValue, key: &str) -> f64 {
    get(value, key).as_f64().expect("analysis number field")
}

fn boolean(value: &JsValue, key: &str) -> bool {
    get(value, key).as_bool().expect("analysis boolean field")
}

/// What a pass that never reached its end managed to publish, so a deadline
/// says whether the analysis stalled at the start or merely ran out of budget.
fn progress_report(events: &Events, diagnostics: &[BroadcastObservation; 2]) -> String {
    let received = events.borrow();
    let publications = match received.last() {
        Some(last) => format!(
            "{} publication(s), last revision {} over {} source frame(s), settled={}, beatFinal={}",
            received.len(),
            number(last, "revision"),
            number(last, "sourceFrames"),
            boolean(last, "settled"),
            boolean(last, "beatFinal"),
        ),
        None => "no publication at all".to_string(),
    };
    format!(
        "{publications}; kithara-reply={}; kithara-events (analysis arrays are lengths)={}",
        diagnostics[0].report(),
        diagnostics[1].report(),
    )
}

fn revisions(events: &Events) -> Vec<f64> {
    events
        .borrow()
        .iter()
        .map(|event| number(event, "revision"))
        .collect()
}

fn observed_player() -> (AudioPlayer, Events, [BroadcastObservation; 2]) {
    static BINDINGS: Once = Once::new();
    BINDINGS.call_once(|| {
        kithara_ffi::web::bindings::setup().expect("browser bindings initialized");
    });
    let diagnostics = [
        BroadcastObservation::new("kithara-reply"),
        BroadcastObservation::new("kithara-events"),
    ];
    let events: Events = Rc::new(RefCell::new(Vec::new()));
    let player = AudioPlayer::new_js();

    let sink = Rc::clone(&events);
    let observer = Closure::wrap(Box::new(move |event: JsValue| {
        sink.borrow_mut().push(event);
    }) as Box<dyn FnMut(JsValue)>);
    player
        .set_analysis_observer_js(observer.as_ref().clone())
        .expect("analysis observer accepted");
    observer.forget();
    (player, events, diagnostics)
}

fn clicks_url() -> String {
    format!("{SERVER_URL}{CLICKS_PATH}")
}

#[wasm_bindgen_test]
async fn analysis_observers_ignore_other_players_tracks() {
    let (other, other_events, _) = observed_player();
    let (analyzed, events, diagnostics) = observed_player();
    let other_id = other.append_js(clicks_url()).expect("append other item");
    let analyzed_id = analyzed
        .append_js(clicks_url())
        .expect("append analyzed item");
    assert_ne!(other_id, analyzed_id);
    assert!(
        drive(&analyzed, &events, |_| {
            other.item_count() == 1 && analyzed.item_count() == 1
        })
        .await
        .is_some(),
        "both owners must admit their items; {}",
        progress_report(&events, &diagnostics)
    );
    analyzed.analyze_js(analyzed_id).expect("analyze accepted");
    assert!(
        drive(&analyzed, &events, |received| {
            received
                .last()
                .is_some_and(|event| boolean(event, "settled") && boolean(event, "beatFinal"))
        })
        .await
        .is_some(),
        "the analyzed player must publish its result; {}",
        progress_report(&events, &diagnostics)
    );
    assert_decoded_clicks(&events, analyzed_id);
    other.remove_all_items();
    analyzed.remove_all_items();
    assert!(
        drive(&analyzed, &events, |_| {
            other.item_count() == 0 && analyzed.item_count() == 0
        })
        .await
        .is_some(),
        "both owners must remove their items"
    );
    let foreign_publications = other_events.borrow().len();
    assert_eq!(
        foreign_publications, 0,
        "a player must not receive another player's analysis"
    );
}

#[wasm_bindgen_test]
async fn a_queued_track_publishes_analysis_until_the_pass_settles() {
    let (player, events, diagnostics) = observed_player();
    let track_id = player.append_js(clicks_url()).expect("append");
    player.analyze_js(track_id).expect("analyze accepted");

    let settled = drive(&player, &events, |received| {
        received
            .last()
            .is_some_and(|e| boolean(e, "settled") && boolean(e, "beatFinal"))
    })
    .await;

    let Some(waited) = settled else {
        panic!(
            "no publication carries a final grid; {}",
            progress_report(&events, &diagnostics)
        );
    };

    let received = events.borrow();
    let last = received.last().expect("at least one publication");

    assert_eq!(number(last, "trackId"), track_id);
    assert!(boolean(last, "settled"));
    assert!(number(last, "sampleRate") > 0.0);
    assert!(number(last, "sourceFrames") > 0.0);
    assert_eq!(
        keys(last),
        [
            "beatFinal",
            "beats",
            "bpm",
            "downbeats",
            "revision",
            "sampleRate",
            "settled",
            "sourceFrames",
            "trackId",
            "waveform",
        ],
        "the callback receives the published fields and nothing else"
    );

    let waveform: Float32Array = get(last, "waveform").dyn_into().expect("waveform copy");
    assert!(waveform.length() > 0, "a settled pass has a waveform");
    assert_eq!(
        waveform.length() % 3,
        0,
        "a bucket is three band energies wide"
    );

    let beats: Float64Array = get(last, "beats").dyn_into().expect("beats copy");
    let downbeats: Float64Array = get(last, "downbeats").dyn_into().expect("downbeats copy");
    assert_eq!(
        downbeats.length(),
        0,
        "the DSP backend reports beats, never downbeats"
    );
    for index in 0..beats.length() {
        let at = beats.get_index(index);
        assert!(at >= 0.0, "a beat sits on the source timeline");
    }
    let bpm = number(last, "bpm");
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "measured bpm {bpm} over {} beats, settled after {waited} ms",
        beats.length()
    )));
    let _ = boolean(last, "beatFinal");

    let revisions = revisions(&events);
    for pair in revisions.windows(2) {
        assert!(
            pair[1] > pair[0],
            "every publication outranks the one before it: {revisions:?}"
        );
    }
}

fn clicks_item() -> Arc<AudioPlayerItem> {
    AudioPlayerItem::new(FfiItemConfig {
        url: clicks_url(),
        abr_mode: None,
        audio_id: None,
        headers: None,
        uuid_i64: None,
        is_live_stream: false,
        preferred_peak_bitrate: 0.0,
        preferred_peak_bitrate_expensive: 0.0,
    })
}

fn assert_decoded_clicks(events: &Events, track_id: f64) {
    let received = events.borrow();
    assert!(
        received
            .iter()
            .all(|event| number(event, "trackId") == track_id)
    );
    let settled = received.last().expect("settled replacement analysis");
    assert!(number(settled, "sampleRate") > 0.0);
    assert!(number(settled, "sourceFrames") > 0.0);
    let waveform: Float32Array = get(settled, "waveform").dyn_into().expect("waveform");
    let energies = waveform.to_vec();
    assert!(!energies.is_empty());
    assert_eq!(energies.len() % 3, 0);
    assert!(energies.iter().all(|energy| energy.is_finite()));
    assert!(
        energies.iter().any(|energy| *energy > 0.0),
        "actual decoded waveform"
    );
    let beats: Float64Array = get(settled, "beats").dyn_into().expect("beats");
    assert!(
        beats.length() > 0,
        "the click fixture yields an actual beat grid"
    );
}

#[wasm_bindgen_test]
async fn partial_replacement_keeps_the_accepted_target_and_publishes_its_analysis() {
    let (player, events, diagnostics) = observed_player();
    let player = Arc::new(player);
    player.append_js(clicks_url()).expect("append old item");
    assert!(
        drive(&player, &events, |_| player.item_count() == 1)
            .await
            .is_some(),
        "the owner never admitted the old item; {}",
        progress_report(&events, &diagnostics)
    );
    let old = player.items().pop().expect("published old item");
    let old_id = old.queue_id();
    let old_ref = Arc::downgrade(&old);
    let replacement = clicks_item();
    let replacement_id = replacement.queue_id();
    let replacement_raw =
        f64::from(u32::try_from(replacement_id.as_u64()).expect("fixture routing id fits u32"));
    let replacement_ref = Arc::downgrade(&replacement);
    let player_events: Events = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&player_events);
    let observer = Closure::wrap(Box::new(move |event: JsValue| {
        sink.borrow_mut().push(event);
    }) as Box<dyn FnMut(JsValue)>);
    player
        .set_observer_js(observer.as_ref().clone())
        .expect("player observer accepted");
    observer.forget();

    player
        .replace_item(0, replacement)
        .expect("post replacement insertion");
    player
        .analyze_js(replacement_raw)
        .expect("analysis waits for the pending composite replacement");
    // This removal runs before Replace can post its second Remove.
    player.remove(&old).expect("post old-item removal");
    assert_eq!(player.items()[0].queue_id(), old_id);
    assert_eq!(
        player.item_count(),
        1,
        "posting preserves the owner snapshot"
    );
    assert!(
        replacement_ref.upgrade().is_some(),
        "pending replacement custody"
    );
    drop(old);

    let expected = QueueError::UnknownTrackId(old_id).to_string();
    assert!(
        drive(&player, &events, |received| {
            let replacement_is_present = player
                .items()
                .as_slice()
                .first()
                .is_some_and(|item| item.queue_id() == replacement_id)
                && player.item_count() == 1;
            let refused_second_remove = player_events.borrow().iter().any(|event| {
                get(event, "kind").as_string().as_deref() == Some("Error")
                    && get(event, "error").as_string().as_deref() == Some(expected.as_str())
            });
            replacement_is_present
                && refused_second_remove
                && old_ref.upgrade().is_none()
                && received.last().is_some_and(|event| {
                    number(event, "trackId") == replacement_raw
                        && boolean(event, "settled")
                        && boolean(event, "beatFinal")
                })
        })
        .await
        .is_some(),
        "partial replacement did not retain and analyze its accepted target; {}; player events={:?}",
        progress_report(&events, &diagnostics),
        &*player_events.borrow()
    );
    assert!(
        replacement_ref.upgrade().is_some(),
        "confirmed replacement custody"
    );
    let errors: Vec<_> = player_events
        .borrow()
        .iter()
        .filter(|event| get(event, "kind").as_string().as_deref() == Some("Error"))
        .map(|event| get(event, "error").as_string().expect("owner error text"))
        .collect();
    assert_eq!(
        errors,
        [expected],
        "only the composite's second removal is refused"
    );
    assert_decoded_clicks(&events, replacement_raw);

    player.remove_all_items();
    assert!(
        drive(&player, &events, |_| {
            player.item_count() == 0 && replacement_ref.upgrade().is_none()
        })
        .await
        .is_some(),
        "published removal releases the accepted replacement object"
    );
}

#[wasm_bindgen_test]
async fn analyzing_again_starts_a_new_revision_sequence_and_silences_the_old_pass() {
    let (player, events, diagnostics) = observed_player();
    let track_id = player.append_js(clicks_url()).expect("append");
    player.analyze_js(track_id).expect("analyze accepted");

    let settled = drive(&player, &events, |received| {
        received.last().is_some_and(|e| boolean(e, "settled"))
    })
    .await;
    assert!(
        settled.is_some(),
        "the first pass never settled after {DEADLINE_MS} ms; {}",
        progress_report(&events, &diagnostics)
    );

    let before = events.borrow().len();
    player
        .analyze_js(track_id)
        .expect("second analyze accepted");
    let restarted = drive(&player, &events, |received| {
        received.len() >= before + RESTART_SAMPLE
    })
    .await;
    assert!(
        restarted.is_some(),
        "the restarted pass published {} time(s); {}",
        events.borrow().len() - before,
        progress_report(&events, &diagnostics)
    );

    let revisions = revisions(&events);
    let suffix = &revisions[before..];
    let steps_down: Vec<usize> = (1..suffix.len())
        .filter(|&index| suffix[index] < suffix[index - 1])
        .collect();
    assert!(
        steps_down.len() <= 1,
        "a restart begins one new revision sequence: {suffix:?}"
    );
    let restart_at = steps_down.first().copied().unwrap_or(0);
    for pair in suffix[restart_at..].windows(2) {
        assert!(
            pair[1] > pair[0],
            "the restarted pass only moves forward: {suffix:?}"
        );
    }
}

fn keys(value: &JsValue) -> Vec<String> {
    let mut keys = Reflect::own_keys(value)
        .expect("analysis keys")
        .iter()
        .map(|key| key.as_string().expect("string key"))
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

async fn drive<F>(player: &AudioPlayer, events: &Events, done: F) -> Option<u64>
where
    F: Fn(&[JsValue]) -> bool,
{
    let mut waited = 0;
    while waited < DEADLINE_MS {
        player.tick_js();
        sleep(Duration::from_millis(POLL_MS)).await;
        waited += POLL_MS;

        let received = events.borrow();
        if done(&received) {
            return Some(waited);
        }
    }
    None
}
