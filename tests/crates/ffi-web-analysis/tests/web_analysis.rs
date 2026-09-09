#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Float32Array, Float64Array, Reflect};
use kithara::platform::time::{Duration, sleep};
use kithara_ffi::player::AudioPlayer;
use kithara_test_fixtures::SignalAsset;
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

const SERVER_URL: &str = "http://127.0.0.1:3444";
const POLL_MS: u64 = 50;
const IDLE_GIVE_UP_MS: u64 = 5_000;
const DEADLINE_MS: u64 = 10_000;
const RESTART_SAMPLE: usize = 3;

type Events = Rc<RefCell<Vec<JsValue>>>;

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).expect("analysis field")
}

fn number(value: &JsValue, key: &str) -> f64 {
    get(value, key).as_f64().expect("analysis number field")
}

fn boolean(value: &JsValue, key: &str) -> bool {
    get(value, key).as_bool().expect("analysis boolean field")
}

fn revisions(events: &Events) -> Vec<f64> {
    events
        .borrow()
        .iter()
        .map(|event| number(event, "revision"))
        .collect()
}

fn observed_player() -> (AudioPlayer, Events) {
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
    (player, events)
}

fn clicks_url() -> String {
    format!("{SERVER_URL}{}", SignalAsset::MP3_CLICKS126_30S.path())
}

#[wasm_bindgen_test]
async fn a_queued_track_publishes_analysis_until_the_pass_settles() {
    let (player, events) = observed_player();
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
            "no publication carries a final grid; {} publication(s) received",
            events.borrow().len()
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

#[wasm_bindgen_test]
async fn analyzing_again_starts_a_new_revision_sequence_and_silences_the_old_pass() {
    let (player, events) = observed_player();
    let track_id = player.append_js(clicks_url()).expect("append");
    player.analyze_js(track_id).expect("analyze accepted");

    let settled = drive(&player, &events, |received| {
        received.last().is_some_and(|e| boolean(e, "settled"))
    })
    .await;
    assert!(settled.is_some(), "the first pass never settled");

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
        "the restarted pass published {} time(s)",
        events.borrow().len() - before
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
    let mut idle = 0;
    let mut seen = 0;
    while waited < DEADLINE_MS && idle < IDLE_GIVE_UP_MS {
        player.tick_js();
        sleep(Duration::from_millis(POLL_MS)).await;
        waited += POLL_MS;

        let received = events.borrow();
        if done(&received) {
            return Some(waited);
        }
        if received.len() == seen {
            idle += POLL_MS;
            continue;
        }
        seen = received.len();
        idle = 0;
    }
    None
}
