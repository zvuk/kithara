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
//! product web session opens its `AudioContext` on the main thread only, so its
//! tests get a binary whose flag says so and open one session at a time.

use std::num::NonZeroU32;

use kithara::{
    assets::{AssetStore, StorageBackend},
    host::{CrossfaderBus, Host, HostConfig, HostOwned, crossfader_gain, wasm},
    output::OutputGroup,
    platform::{
        sync::{
            Arc,
            atomic::{AtomicU32, AtomicU64, Ordering},
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
};
use kithara_test_fixtures::{
    SignalAsset,
    signal::{deinterleave_left, rms},
};
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
                .select_item(0, kithara::play::SelectionPlayback::Play)
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

const DECK_TONES_HZ: [f64; 2] = [440.0, 880.0];
const DECK_ASSETS: [SignalAsset; 2] = [SignalAsset::MP3_SINE440_60S, SignalAsset::MP3_SINE880_30S];

struct DeckPair {
    requested: AtomicU32,
    applied: AtomicU32,
    positions: [AtomicU64; 2],
    stage: AtomicU64,
}

impl DeckPair {
    const UNAPPLIED: u32 = u32::MAX;
    const STAGES: [&str; 4] = [
        "spawning the Worker",
        "opening both decks",
        "ticking both decks",
        "past the tick loop",
    ];

    fn new(position: f32) -> Self {
        Self {
            requested: AtomicU32::new(position.to_bits()),
            applied: AtomicU32::new(Self::UNAPPLIED),
            positions: [AtomicU64::new(0), AtomicU64::new(0)],
            stage: AtomicU64::new(0),
        }
    }

    fn is_applied(&self, position: f32) -> bool {
        self.applied.load(Ordering::Acquire) == position.to_bits()
    }

    fn positions(&self) -> [f64; 2] {
        [0, 1].map(|deck| f64::from_bits(self.positions[deck].load(Ordering::Relaxed)))
    }

    fn stage(&self) -> &'static str {
        usize::try_from(self.stage.load(Ordering::Relaxed))
            .ok()
            .and_then(|index| Self::STAGES.get(index))
            .copied()
            .unwrap_or("an unknown stage")
    }
}

async fn open_deck(
    host: &mut Host<TestPools>,
    server: &TestServerHelper,
    worker: &PlayWorker<TestPools>,
    store: &AssetStore<TestPools>,
    asset: SignalAsset,
) -> HostOwned<PlayerImpl<TestPools>> {
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(host.requested_sample_rate())
            .worker(worker.clone())
            .build(),
    );
    let owner = host.insert(player).expect("insert the deck into the Host");
    let control = owner.control().clone();
    control.set_crossfade_duration(0.0);
    let url = server.signal(asset);
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse(url.as_str()).expect("the fixture URL parses as a resource source"),
    )
    .worker(worker.clone())
    .store(store.clone())
    .build();
    let mut resource = Resource::new(config)
        .await
        .expect("open the fixture as a product resource");
    resource.preload().await.expect("preload the fixture");
    control.insert(resource, TrackId(0), None);
    control
        .select_item(0, kithara::play::SelectionPlayback::Pause)
        .expect("select the fixture");
    owner
}

fn spawn_deck_pair_worker(sender: wasm::HostSender<TestPools>, pair: Arc<DeckPair>) {
    spawn_named("live-web-audio-decks", move || {
        keep_worker_alive();
        task::spawn(async move {
            pair.stage.store(1, Ordering::Relaxed);
            let mut host = wasm::remote_host(sender);
            let region = pools();
            let worker = PlayWorker::new(PlayWorkerConfig::builder(region.clone()).build());
            let store = AssetStore::builder(region)
                .backend(StorageBackend::Memory)
                .build();
            let server = TestServerHelper::new().await;
            let mut decks = Vec::with_capacity(DECK_ASSETS.len());
            for asset in DECK_ASSETS {
                decks.push(open_deck(&mut host, &server, &worker, &store, asset).await);
            }
            pair.stage.store(2, Ordering::Relaxed);

            let mut applied = DeckPair::UNAPPLIED;
            loop {
                let requested = pair.requested.load(Ordering::Acquire);
                if requested != applied {
                    let position = f32::from_bits(requested);
                    let levels = [CrossfaderBus::A, CrossfaderBus::B]
                        .map(|bus| crossfader_gain(bus, position).expect("a valid position"));
                    host.apply_mix(
                        decks
                            .iter()
                            .zip(levels)
                            .map(|(deck, level)| deck.level(level)),
                    )
                    .expect("apply the crossfader batch");
                    if applied == DeckPair::UNAPPLIED {
                        for deck in &decks {
                            deck.control().play();
                        }
                    }
                    applied = requested;
                    pair.applied.store(applied, Ordering::Release);
                }
                for (deck, position) in decks.iter().zip(&pair.positions) {
                    if deck.control().tick().is_err() {
                        pair.stage.store(3, Ordering::Relaxed);
                        return;
                    }
                    let seconds = deck.control().position_seconds().unwrap_or_default();
                    position.store(seconds.to_bits(), Ordering::Relaxed);
                }
                sleep(WORKER_TICK).await;
            }
        });
    });
}

async fn hear(
    receiver: &wasm::HostReceiver<TestPools>,
    tap: &mut HeapCons<f32>,
    frames: usize,
    deadline: f64,
) -> Vec<f32> {
    let page = web_sys::window().expect("the browser main thread owns a window");
    let document = page.document().expect("the runner page owns a document");
    let target = frames * CHANNELS;
    let mut window: Vec<f32> = Vec::with_capacity(target);
    while js_sys::Date::now() < deadline && window.len() < target {
        wasm::tick_and_poll(receiver);
        let click = web_sys::Event::new("click").expect("build a click event");
        document.dispatch_event(&click).expect("dispatch the click");
        next_frame(&page).await;
        collect_tap(tap, &mut window);
    }
    window
}

async fn until_applied(
    receiver: &wasm::HostReceiver<TestPools>,
    pair: &DeckPair,
    position: f32,
    deadline: f64,
) {
    let page = web_sys::window().expect("the browser main thread owns a window");
    while js_sys::Date::now() < deadline && !pair.is_applied(position) {
        wasm::tick_and_poll(receiver);
        next_frame(&page).await;
    }
    assert!(
        pair.is_applied(position),
        "the Worker did not apply crossfader position {position} while it was {}",
        pair.stage()
    );
}

fn tone_and_level(window: &[f32], measured: u32) -> (f64, f32) {
    let half = usize::try_from(measured).unwrap() / 2 * CHANNELS;
    let steady = &window[half.min(window.len())..];
    (
        zero_crossing_hz(&deinterleave_left(steady, CHANNELS), f64::from(measured)),
        rms(steady),
    )
}

#[wasm_bindgen_test]
async fn two_decks_in_one_host_follow_the_crossfader() {
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

    let pair = Arc::new(DeckPair::new(0.0));
    spawn_deck_pair_worker(sender, Arc::clone(&pair));
    let deadline = js_sys::Date::now() + 2.0 * AUDIO_BUDGET_MS;
    let second = usize::try_from(RATE.get()).unwrap();

    until_applied(&receiver, &pair, 0.0, deadline).await;
    let toward_a = hear(&receiver, &mut pcm_rx, second, deadline).await;
    let at_a = pair.positions();

    pair.requested.store(1.0_f32.to_bits(), Ordering::Release);
    until_applied(&receiver, &pair, 1.0, deadline).await;
    let _ = pcm_rx.pop_iter().count();
    let toward_b = hear(&receiver, &mut pcm_rx, second, deadline).await;
    let at_b = pair.positions();

    let dropped = drops.load(Ordering::Relaxed);
    let rates = host.sample_rate().expect("read the session output rate");
    assert_eq!(
        (rates.requested, rates.measured),
        (RATE.get(), Some(RATE.get())),
        "the session lost the rate it asked for while the Worker was {}",
        pair.stage()
    );
    let measured = rates.output();
    for (label, window) in [("A", &toward_a), ("B", &toward_b)] {
        assert!(
            window.len() / CHANNELS >= second,
            "toward {label} the tap carried {} frames of {second}, {dropped} dropped, while the \
             Worker was {}",
            window.len() / CHANNELS,
            pair.stage()
        );
    }
    assert_eq!(dropped, 0, "tap dropped samples");

    for ((label, window), expected) in [("A", &toward_a), ("B", &toward_b)]
        .into_iter()
        .zip(DECK_TONES_HZ)
    {
        let (tone, level) = tone_and_level(window, measured);
        tracing::info!(label, tone, level, "crossfader window");
        assert!(
            (tone - expected).abs() <= expected * TONE_TOLERANCE,
            "toward {label} the tap carries {tone:.1} Hz, not {expected} Hz"
        );
        assert!(
            (MIN_RMS..=MAX_RMS).contains(&level),
            "toward {label} one full-scale deck plays at RMS {level:.4}, outside \
             {MIN_RMS}..={MAX_RMS}"
        );
    }
    for deck in 0..2 {
        assert!(
            at_a[deck] > 0.0 && at_b[deck] > at_a[deck],
            "deck {deck} position went {} -> {} across the crossfade",
            at_a[deck],
            at_b[deck]
        );
    }
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
// Every way this probe can end is a state the worklet reaches: the module
// fails to load, the processor reports an error, or it posts what it saw.
// A timer racing those would report a slow browser as a silent one, and the
// run already has a deadline of its own.
const reported = context.audioWorklet.addModule(url).then(() => new Promise((resolve) => {
  const node = new AudioWorkletNode(context, "kithara-diagnostic-probe", {
    processorOptions: [wasmModule, wasmMemory, probeName],
  });
  node.onprocessorerror = () => resolve("the worklet errored before it reported");
  node.port.onmessage = (event) => resolve(event.data);
}));
return reported.finally(() => {
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
