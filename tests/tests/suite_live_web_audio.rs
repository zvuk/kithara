#![forbid(unsafe_code)]
#![cfg(target_arch = "wasm32")]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]
//! Live playback through the product web graph, heard on the browser main
//! thread's mix tap and measured against the rate the session asked the device
//! for.
//!
//! `wasm-bindgen-test` picks its thread from a link-section flag that covers a
//! whole binary, and `kithara::test` sets that flag to a dedicated worker. The
//! product web session opens its `AudioContext` on the main thread only, so it
//! gets a binary whose flag says so, and stays the only session in it.

use std::num::NonZeroU32;

use kithara::{
    assets::{AssetStore, StorageBackend},
    host::{Host, HostConfig, wasm},
    output::OutputGroup,
    platform::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        thread::{keep_worker_alive, spawn_named},
        time::{Duration, sleep},
        tokio::task,
    },
    play::{
        MixTapWriter, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, Resource,
        ResourceConfig, ResourceSrc,
    },
    queue::TrackId,
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    offline::{deinterleave_left, rms},
};
use kithara_test_fixtures::SignalAsset;
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Split},
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// Console output for both halves of the graph: `tracing` from the workspace,
/// and `log` from the Web Audio backend that reports its own failures there.
fn init_diagnostics() {
    kithara::platform::logging::install_panic_hook();
    let _ = tracing_log::LogTracer::init();
    kithara_test_utils::test::setup_tracing();
}

const RATE: NonZeroU32 = NonZeroU32::new(44_100).unwrap();
const CHANNELS: usize = 2;
/// Eight seconds of stereo headroom, so a frame that arrives late still finds
/// the ring with room.
const TAP_CAPACITY: usize = 8 * 44_100 * CHANNELS;
/// The cadence the product Worker ticks its queue at.
const WORKER_TICK: Duration = Duration::from_millis(100);
/// How long the tap may take to carry its second of audio.
const AUDIO_BUDGET_MS: f64 = 15_000.0;
const AUDIBLE_THRESHOLD: f32 = 0.01;
const TONE_HZ: f64 = 440.0;
const TONE_TOLERANCE: f64 = 0.02;
/// The signal route serves every tone at full scale, so a sine has RMS
/// `1/sqrt(2)`; the band leaves room for the limiter and mp3 framing.
const MIN_RMS: f32 = 0.55;
const MAX_RMS: f32 = 0.75;

/// How far the player Worker got. Its console is not the one
/// `wasm-bindgen-test` captures, so a failure there reads only from here.
const STAGES: [&str; 8] = [
    "spawning the Worker",
    "inserting the player into the Host",
    "starting the test server",
    "opening the fixture resource",
    "preloading the fixture",
    "starting playback",
    "ticking the queue",
    "past the tick loop",
];

fn stage_name(stage: u64) -> &'static str {
    usize::try_from(stage)
        .ok()
        .and_then(|index| STAGES.get(index))
        .copied()
        .unwrap_or("an unknown stage")
}

/// One player on the fixture sine, owned by the Worker the wasm Host requires
/// for insertion. It keeps ticking until the test's Host route closes.
fn spawn_player_worker(sender: wasm::HostSender<TestPools>, stage: Arc<AtomicU64>) {
    spawn_named("live-web-audio-player", move || {
        keep_worker_alive();
        task::spawn(async move {
            stage.store(1, Ordering::Relaxed);
            let mut host = wasm::remote_host(sender);
            let region = pools();
            let worker = PlayWorker::new(PlayWorkerConfig::builder(region.clone()).build());
            let player = PlayerImpl::new(
                PlayerConfig::builder()
                    .sample_rate(host.requested_sample_rate())
                    .worker(worker.clone())
                    .build(),
            );
            let owner = host
                .insert(player)
                .expect("insert the player into the Host");
            stage.store(2, Ordering::Relaxed);
            let control = owner.control().clone();
            control.set_crossfade_duration(0.0);

            let server = TestServerHelper::new().await;
            stage.store(3, Ordering::Relaxed);
            let url = server.signal(SignalAsset::MP3_SINE440_60S);
            let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
                ResourceSrc::parse(url.as_str())
                    .expect("the fixture URL parses as a resource source"),
            )
            .worker(worker)
            .store(
                AssetStore::builder(region)
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build();
            let mut resource = Resource::new(config)
                .await
                .expect("open the fixture as a product resource");
            stage.store(4, Ordering::Relaxed);
            resource.preload().await.expect("preload the fixture");
            stage.store(5, Ordering::Relaxed);

            control.insert(resource, TrackId(0), None);
            control
                .select_item(0, true)
                .expect("select the fixture for playback");
            control.play();
            stage.store(6, Ordering::Relaxed);

            while control.tick().is_ok() {
                sleep(WORKER_TICK).await;
            }
            stage.store(7, Ordering::Relaxed);
        });
    });
}

/// One `requestAnimationFrame` turn, the cadence `tick_and_poll` documents.
async fn next_frame(window: &web_sys::Window) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let frame = Closure::once_into_js(move |_: JsValue| {
            let _ = resolve.call0(&JsValue::NULL);
        });
        window
            .request_animation_frame(frame.unchecked_ref())
            .expect("schedule the next animation frame");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Append what the tap holds, starting the window at the first audible frame.
fn collect_tap(tap: &mut HeapCons<f32>, window: &mut Vec<f32>) -> usize {
    let drained: Vec<f32> = tap.pop_iter().collect();
    if window.is_empty() {
        if let Some(start) = first_audible_sample(&drained) {
            window.extend_from_slice(&drained[start..]);
        }
    } else {
        window.extend_from_slice(&drained);
    }
    drained.len()
}

fn first_audible_sample(samples: &[f32]) -> Option<usize> {
    samples
        .chunks_exact(CHANNELS)
        .position(|frame| frame.iter().any(|sample| sample.abs() > AUDIBLE_THRESHOLD))
        .map(|frame| frame * CHANNELS)
}

fn session_is_live(host: &Host<TestPools>) -> bool {
    host.sample_rate()
        .is_ok_and(|rates| rates.measured == Some(rates.requested))
}

/// Dominant frequency of a mono window, taken from its rising zero crossings.
fn zero_crossing_hz(mono: &[f32], sample_rate: f64) -> f64 {
    let rising: Vec<usize> = mono
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] <= 0.0 && pair[1] > 0.0)
        .map(|(index, _)| index)
        .collect();
    let (Some(&first), Some(&last)) = (rising.first(), rising.last()) else {
        return 0.0;
    };
    if last == first {
        return 0.0;
    }
    let periods = f64::from(u32::try_from(rising.len() - 1).unwrap());
    let span = f64::from(u32::try_from(last - first).unwrap());
    periods * sample_rate / span
}

#[wasm_bindgen_test]
async fn live_web_audio_plays_at_session_rate() {
    init_diagnostics();

    let host: Host<TestPools> = Host::new(HostConfig::builder().sample_rate_hint(RATE).build())
        .expect("build the product web Host");
    let (sender, receiver) = wasm::worker_host_channel(&host).expect("open the Worker route");
    wasm::warm_up_audio(&host).expect("warm up the audio context");

    let (pcm_tx, mut pcm_rx) = HeapRb::<f32>::new(TAP_CAPACITY).split();
    let drops = Arc::new(AtomicU64::new(0));
    let mut outputs = OutputGroup::new();
    outputs.push(MixTapWriter::new(pcm_tx, Arc::clone(&drops)));
    host.enable_outputs(outputs).expect("install the mix tap");

    let stage = Arc::new(AtomicU64::new(0));
    spawn_player_worker(sender, Arc::clone(&stage));

    let page = web_sys::window().expect("the browser main thread owns a window");
    let document = page.document().expect("the runner page owns a document");
    let target = usize::try_from(RATE.get()).unwrap() * CHANNELS;
    let mut window: Vec<f32> = Vec::with_capacity(target);
    let mut frames_pumped = 0u64;
    let mut tapped = 0usize;
    let deadline = js_sys::Date::now() + AUDIO_BUDGET_MS;
    let mut opened = false;
    while js_sys::Date::now() < deadline && window.len() < target {
        wasm::tick_and_poll(&receiver);
        // Firewheel resumes a suspended `AudioContext` only from a document
        // interaction event, and the Worker installs that listener mid-run.
        let click = web_sys::Event::new("click").expect("build a click event");
        document.dispatch_event(&click).expect("dispatch the click");
        next_frame(&page).await;
        tapped += collect_tap(&mut pcm_rx, &mut window);
        frames_pumped += 1;
        // A session that reported its rate and then stopped has lost its stream,
        // and waiting out the budget would report silence instead of the drop.
        if session_is_live(&host) {
            opened = true;
        } else if opened {
            break;
        }
    }

    let frames = window.len() / CHANNELS;
    let dropped = drops.load(Ordering::Relaxed);
    let stage = stage_name(stage.load(Ordering::Relaxed));
    let rates = host.sample_rate().expect("read the session output rate");
    assert_eq!(
        (rates.requested, rates.measured),
        (RATE.get(), Some(RATE.get())),
        "the session lost the rate it asked for after {frames_pumped} frames, having tapped \
         {frames} frames with {dropped} dropped while the Worker was {stage}"
    );

    let measured = rates.output();
    let second = usize::try_from(measured).unwrap();
    assert!(
        frames >= second,
        "tap carried {frames} frames of the {second} one second holds at {measured} Hz, \
         with {dropped} dropped over {frames_pumped} frames and {tapped} samples tapped, \
         while the Worker was {stage}"
    );
    assert_eq!(
        dropped, 0,
        "tap dropped samples while carrying {frames} frames"
    );

    let steady = &window[(second / 2 * CHANNELS)..];
    let tone = zero_crossing_hz(&deinterleave_left(steady, CHANNELS), f64::from(measured));
    let level = rms(steady);
    tracing::info!(
        frames_pumped,
        frames,
        measured,
        tone,
        level,
        "live web audio window"
    );
    assert!(
        (tone - TONE_HZ).abs() <= TONE_HZ * TONE_TOLERANCE,
        "tap carries {tone:.1} Hz, further than {TONE_TOLERANCE} from {TONE_HZ} Hz"
    );
    assert!(
        (MIN_RMS..=MAX_RMS).contains(&level),
        "full-scale sine plays at RMS {level:.4}, outside {MIN_RMS}..={MAX_RMS}"
    );
}

/// A diagnostic that spans several `String.fromCharCode` calls and puts a
/// surrogate pair on the boundary between two of them.
fn diagnostic_probe_message() -> String {
    let mut message = "a".repeat(1023);
    message.push('\u{1F600}');
    message.push_str("tail");
    message
}

/// Reached from inside an `AudioWorkletGlobalScope`, the scope the audio render
/// pass runs in, through the bindgen module the worklet imports.
#[wasm_bindgen]
pub fn kithara_worklet_diagnostic_probe() {
    kithara::platform::logging::log_error(&diagnostic_probe_message());
}

const PANIC_PROBE_PAYLOAD: &str = "the render scope reports a panic";

/// Panics in the render scope. `init_diagnostics` fills the process-wide hook
/// slot from the main thread, so this pins that a panic in another instance of
/// the module reports through it, in the console of its own realm.
#[wasm_bindgen]
pub fn kithara_worklet_panic_probe() {
    panic!("{PANIC_PROBE_PAYLOAD}");
}

/// Loads the bindgen module into an `AudioWorkletGlobalScope`, calls the named
/// export there and resolves to what `console.error` received. A call that
/// traps still resolves: the diagnostic under test is written before the trap.
const WORKLET_PROBE: &str = r#"
const source = `
registerProcessor("kithara-diagnostic-probe", class extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const [module, memory, probe] = options.processorOptions;
    let thrown = "";
    const reported = [];
    console.error = (line) => reported.push(line);
    try {
      bindgen.initSync({ module, memory, thread_stack_size: 1048576 });
      bindgen[probe]();
    } catch (error) {
      thrown = "threw: " + String(error);
    }
    this.port.postMessage(reported.length ? reported.join("") : thrown);
  }
  process() { return false; }
});`;
const url = URL.createObjectURL(new Blob(
  ["import init, * as bindgen from '" + moduleUrl + "';\n\n", source],
  { type: "text/javascript" },
));
const context = new AudioContext();
const reported = context.audioWorklet.addModule(url).then(() => new Promise((resolve) => {
  const node = new AudioWorkletNode(context, "kithara-diagnostic-probe", {
    processorOptions: [wasmModule, wasmMemory, probeName],
  });
  node.port.onmessage = (event) => resolve(event.data);
}));
const deadline = new Promise((resolve) => setTimeout(
  () => resolve("the worklet never reached the diagnostic"),
  2000,
));
return Promise.race([reported, deadline]).finally(() => {
  URL.revokeObjectURL(url);
  context.close();
});
"#;

#[wasm_bindgen]
extern "C" {
    type ImportMeta;

    #[wasm_bindgen(method, getter)]
    fn url(this: &ImportMeta) -> js_sys::JsString;

    #[wasm_bindgen(js_namespace = import, js_name = meta, thread_local_v2)]
    static IMPORT_META: ImportMeta;
}

async fn reported_from_the_render_scope(probe_name: &str) -> String {
    let probe = js_sys::Function::new_with_args(
        "moduleUrl, wasmModule, wasmMemory, probeName",
        WORKLET_PROBE,
    );
    let pending = probe
        .apply(
            &JsValue::NULL,
            &js_sys::Array::of4(
                &IMPORT_META.with(ImportMeta::url).into(),
                &wasm_bindgen::module(),
                &wasm_bindgen::memory(),
                &JsValue::from_str(probe_name),
            ),
        )
        .expect("start the worklet probe");

    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(pending))
        .await
        .expect("await the worklet probe")
        .as_string()
        .unwrap_or_default()
}

#[wasm_bindgen_test]
async fn a_platform_diagnostic_reaches_the_console_from_the_render_scope() {
    let reported = reported_from_the_render_scope("kithara_worklet_diagnostic_probe").await;

    assert_eq!(
        reported,
        diagnostic_probe_message(),
        "the platform diagnostic did not survive the audio render scope"
    );
}

#[wasm_bindgen_test]
async fn a_panic_in_the_render_scope_reports_through_the_hook_the_main_thread_installed() {
    init_diagnostics();

    let reported = reported_from_the_render_scope("kithara_worklet_panic_probe").await;

    assert!(
        reported.starts_with("panicked at ") && reported.ends_with(PANIC_PROBE_PAYLOAD),
        "a panic in the audio render scope reported {reported:?}"
    );
}
