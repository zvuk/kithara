use std::{
    collections::HashMap,
    sync::{
        RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use kithara::platform::{
    flash,
    sync::Arc,
    time::{Duration, sleep},
    tokio::{sync::watch, task::spawn},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    hls_spec::{HlsSpecError, ResolvedHlsSpec, parse_hls_spec_with, resolve_hls_spec_with},
    hls_stream::{GeneratedHls, GeneratedHlsCache, load_hls},
    hls_url::HlsSpec,
};

#[derive(Clone)]
pub enum Content {
    HtmlError(&'static str),
    Status(u16),
    StaticBytes {
        bytes: Arc<Vec<u8>>,
        content_type: Option<&'static str>,
    },
}

#[derive(Clone)]
pub enum Delivery {
    Normal,
    Range,
    EarlyClose {
        after_bytes: usize,
    },
    /// Send `200 OK` headers (with the full `Content-Length`) and the first
    /// `after_bytes` of the body, then never deliver the rest — the
    /// throttling-CDN shape where the connection stays open but goes silent.
    StallAfter {
        after_bytes: usize,
    },
    Throttle {
        chunk: usize,
        delay_ms: u64,
    },
}

/// How every data route answers while the server-wide outage switch is thrown.
///
/// The modes differ in the *error class* the client observes, and that class —
/// not the outage itself — is what a recovery contract turns on: a reachable
/// server answering `503` says "not now", while a dead transport says nothing
/// at all. A fixture that can only produce the former cannot exercise the
/// latter.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Online,
    /// A reachable server refusing to serve: every data route answers `503`.
    Unavailable,
    /// An unreachable network, the shape a device sees in airplane mode: the
    /// response body ends before its declared length, so the client observes a
    /// transport failure instead of an HTTP status. The listener stays bound
    /// because this middleware runs after `accept`; the error class on the
    /// client is what is being modelled, not the kernel-level refusal.
    TransportFailure,
}

#[derive(Clone)]
pub struct FixtureBehavior {
    pub content: Content,
    pub delivery: Delivery,
}

struct BehaviorEntry {
    behavior: FixtureBehavior,
    hits: AtomicU64,
}

pub(crate) struct Gate {
    released: watch::Sender<bool>,
    requested: AtomicU64,
}

impl Gate {
    fn new() -> Self {
        let (released, _rx) = watch::channel(false);
        Self {
            released,
            requested: AtomicU64::new(0),
        }
    }

    pub(crate) fn mark_requested(&self) {
        self.requested.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) async fn wait_until_released(&self) {
        let mut rx = self.released.subscribe();
        let _ = rx.wait_for(|released| *released).await;
    }

    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    pub(crate) fn requested(&self) -> u64 {
        self.requested.load(Ordering::Relaxed)
    }
}

/// A test-controlled withhold gate for one `(hls token, variant, segment)`.
///
/// Two independently-controllable seams:
/// - **Body** (`released`): while unreleased, the segment's GET response parks
///   on `released`; the `requested` counter lets a test observe that the gated
///   GET actually reached the server before it releases. Models "this segment's
///   bytes have not arrived yet".
/// - **Size/HEAD** (`head_withheld`): while set, the segment's HEAD (size)
///   response reports `Content-Length: 0`, so the up-front
///   `loading::size_estimation` pass learns a zero size for it. Models "seek
///   BEFORE this segment's size is known" — the genuinely-immediate-seek
///   condition that the body-only gate cannot model (the body gate would block
///   stream construction, since size estimation HEADs every segment
///   synchronously at `PlayWorker::open`). The `head_requested` counter lets a test
///   observe the HEAD arrival.
///
/// Lives in `TestServerState` (mutable, per-token) — never in the immutable
/// Arc-cached `GeneratedHls`.
#[derive(derive_more::Deref)]
pub(crate) struct SegmentGate {
    #[deref]
    body: Gate,
    head_withheld: AtomicBool,
    head_requested: AtomicU64,
}

impl SegmentGate {
    fn new() -> Self {
        Self {
            body: Gate::new(),
            head_withheld: AtomicBool::new(false),
            head_requested: AtomicU64::new(0),
        }
    }

    /// Mark the segment's HEAD (size) response to report `Content-Length: 0`
    /// until [`Self::release_head`]. Independent of the body GET withhold.
    pub(crate) fn withhold_head(&self) {
        self.head_withheld.store(true, Ordering::Relaxed);
    }

    /// Whether the HEAD (size) response is currently being withheld
    /// (reports `Content-Length: 0`). Observed in-process by the route.
    pub(crate) fn head_is_withheld(&self) -> bool {
        self.head_withheld.load(Ordering::Relaxed)
    }

    /// Reveal the true size on subsequent HEAD (size) requests.
    pub(crate) fn release_head(&self) {
        self.head_withheld.store(false, Ordering::Relaxed);
    }

    /// Mark that a HEAD (size) request reached this gate.
    pub(crate) fn mark_head_requested(&self) {
        self.head_requested.fetch_add(1, Ordering::Relaxed);
    }

    /// In-process count of HEAD (size) requests that reached this gate.
    pub(crate) fn head_requested(&self) -> u64 {
        self.head_requested.load(Ordering::Relaxed)
    }
}

fn segment_gate_key(hls_token: &str, variant: usize, segment: usize) -> String {
    format!("{hls_token}|v{variant}|s{segment}")
}

fn size_probe_key(hls_token: &str, variant: usize, segment: usize) -> String {
    format!("{hls_token}|v{variant}|s{segment}|probe")
}

/// A test-controlled withhold gate for one `(hls token, variant)` init
/// (`EXT-X-MAP`) segment.
///
/// A track's loader does not resolve `TrackStatus::Loaded` until `Resource::new`
/// completes, and `Resource::new` builds the initial decoder through the off-RT
/// **blocking** preparation read (`PlayWorker::open`), whose first read of the
/// container header touches the init body. So while this gate withholds the
/// init GET body, that blocking read parks and the owning track stays in
/// `Loading` until the test releases the gate — a release-driven lever for
/// "this track is still constructing" scenarios that does not depend on
/// wall-clock segment delays (which only gate media-segment bodies fetched at
/// playback, not at construction). The blocking construction read is itself
/// budget-bounded, so the test drives its supersede/select while the gate is
/// held (microseconds against that budget) rather than relying on the gate
/// holding indefinitely.
///
/// One seam only: the init body GET parks on `released` until [`Self::release`].
/// The `requested` counter lets a test observe the gated init GET reached the
/// server. Lives in `TestServerState` (mutable, per-token) — never in the
/// immutable Arc-cached `GeneratedHls`.
pub(crate) type InitGate = Gate;

fn init_gate_key(hls_token: &str, variant: usize) -> String {
    format!("{hls_token}|v{variant}|init")
}

struct RegisteredHls {
    fixture: Arc<GeneratedHls>,
    flash: bool,
}

#[kithara::flash(true)]
async fn response_delay(duration: Duration) {
    sleep(duration).await;
}

type GateMap<T> = RwLock<HashMap<String, Arc<T>>>;

fn register_gate<T>(map: &GateMap<T>, key: String, gate: T) -> Arc<T> {
    let gate = Arc::new(gate);
    map.write()
        .expect("gate map poisoned")
        .insert(key, Arc::clone(&gate));
    gate
}

fn get_gate<T>(map: &GateMap<T>, key: &str) -> Option<Arc<T>> {
    map.read()
        .expect("gate map poisoned")
        .get(key)
        .map(Arc::clone)
}

pub(crate) struct TestServerState {
    hls_cache: GeneratedHlsCache,
    hls_blobs: RwLock<HashMap<String, Arc<Vec<u8>>>>,
    tokens: RwLock<HashMap<String, RegisteredHls>>,
    behaviors: RwLock<HashMap<String, Arc<BehaviorEntry>>>,
    segment_gates: GateMap<SegmentGate>,
    init_gates: GateMap<InitGate>,
    /// Per-`(hls token, variant, segment)` count of size-probe requests the
    /// server has served: every `HEAD` and every single-byte ranged
    /// `GET` (`Range: bytes=0-0`). Unlike the withhold gates this counter is
    /// always live (no pre-registration), so a test can observe the up-front
    /// size-estimation pass that probes every segment of every variant at
    /// `PlayWorker::open` versus the lazy per-segment resolve that probes only
    /// the active prefix.
    size_probes: RwLock<HashMap<String, AtomicU64>>,
    /// Server-wide reachability switch. Anything other than
    /// [`NetworkMode::Online`] models a total network outage rather than one
    /// failed URL. Because it covers the whole server, only a server private to
    /// one test may be taken offline — see `PrivateTestServer`.
    network_mode: RwLock<NetworkMode>,
}

impl TestServerState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: RwLock::new(HashMap::new()),
            hls_cache: RwLock::new(HashMap::new()),
            hls_blobs: RwLock::new(HashMap::new()),
            behaviors: RwLock::new(HashMap::new()),
            segment_gates: GateMap::default(),
            init_gates: GateMap::default(),
            size_probes: RwLock::new(HashMap::new()),
            network_mode: RwLock::new(NetworkMode::Online),
        })
    }

    pub(crate) fn insert_behavior(&self, behavior: FixtureBehavior) -> String {
        let token = Uuid::new_v4().to_string();
        let mut map = self.behaviors.write().expect("behaviors poisoned");
        map.insert(
            token.clone(),
            Arc::new(BehaviorEntry {
                behavior,
                hits: AtomicU64::new(0),
            }),
        );
        token
    }

    pub(crate) fn get_behavior(&self, token: &str) -> Option<FixtureBehavior> {
        let map = self.behaviors.read().expect("behaviors poisoned");
        map.get(token).map(|e| e.behavior.clone())
    }

    pub(crate) fn bump_behavior(&self, token: &str) -> u64 {
        let map = self.behaviors.read().expect("behaviors poisoned");
        match map.get(token) {
            Some(e) => e.hits.fetch_add(1, Ordering::Relaxed) + 1,
            None => 0,
        }
    }

    pub(crate) fn behavior_hits(&self, token: &str) -> Option<u64> {
        let map = self.behaviors.read().expect("behaviors poisoned");
        map.get(token).map(|e| e.hits.load(Ordering::Relaxed))
    }

    pub(crate) fn network_mode(&self) -> NetworkMode {
        *self.network_mode.read().expect("network mode poisoned")
    }

    pub(crate) fn set_network_mode(&self, mode: NetworkMode) {
        *self.network_mode.write().expect("network mode poisoned") = mode;
    }

    /// Register a withhold gate for one `(hls token, variant, segment)` and
    /// return its handle. The matching segment GET parks until [`SegmentGate::release`].
    pub(crate) fn register_segment_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> Arc<SegmentGate> {
        let key = segment_gate_key(hls_token, variant, segment);
        register_gate(&self.segment_gates, key, SegmentGate::new())
    }

    pub(crate) fn segment_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> Option<Arc<SegmentGate>> {
        let key = segment_gate_key(hls_token, variant, segment);
        get_gate(&self.segment_gates, &key)
    }

    /// Register a withhold gate for one `(hls token, variant)` init
    /// (`EXT-X-MAP`) segment and return its handle. The matching init GET
    /// parks until [`InitGate::release`], holding the owning track's loader in
    /// `Loading`.
    pub(crate) fn register_init_gate(&self, hls_token: &str, variant: usize) -> Arc<InitGate> {
        register_gate(
            &self.init_gates,
            init_gate_key(hls_token, variant),
            InitGate::new(),
        )
    }

    pub(crate) fn init_gate(&self, hls_token: &str, variant: usize) -> Option<Arc<InitGate>> {
        get_gate(&self.init_gates, &init_gate_key(hls_token, variant))
    }

    pub(crate) async fn delay_response(&self, hls_token: &str, duration: Duration) {
        let ambient = self
            .tokens
            .read()
            .expect("token store poisoned")
            .get(hls_token)
            .is_some_and(|hls| hls.flash);
        flash::with_ambient(ambient, async {
            spawn(response_delay(duration))
                .await
                .expect("response delay task must complete");
        })
        .await;
    }

    /// Record one size-probe (`HEAD` or single-byte ranged `GET`) served for
    /// `(hls token, variant, segment)`. Always live — no gate registration.
    pub(crate) fn mark_size_probe(&self, hls_token: &str, variant: usize, segment: usize) {
        let key = size_probe_key(hls_token, variant, segment);
        {
            let map = self.size_probes.read().expect("size probes poisoned");
            if let Some(counter) = map.get(&key) {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let mut map = self.size_probes.write().expect("size probes poisoned");
        map.entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Size-probes served for one `(hls token, variant, segment)`.
    pub(crate) fn size_probe_count(&self, hls_token: &str, variant: usize, segment: usize) -> u64 {
        let map = self.size_probes.read().expect("size probes poisoned");
        map.get(&size_probe_key(hls_token, variant, segment))
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
    }

    pub(crate) fn get_hls(&self, token: &str) -> Option<Arc<GeneratedHls>> {
        let store = self.tokens.read().expect("token store poisoned");
        store.get(token).map(|hls| Arc::clone(&hls.fixture))
    }

    pub(crate) fn insert_hls_spec(&self, spec: HlsSpec) -> Result<String, HlsSpecError> {
        let resolved = self.resolve_hls_spec(spec)?;
        let hls = self.load_hls(resolved)?;
        let token = Uuid::new_v4().to_string();
        self.tokens.write().expect("token store poisoned").insert(
            token.clone(),
            RegisteredHls {
                fixture: hls,
                flash: flash::ambient_snapshot(),
            },
        );
        Ok(token)
    }

    pub(crate) fn load_hls(
        &self,
        spec: ResolvedHlsSpec,
    ) -> Result<Arc<GeneratedHls>, HlsSpecError> {
        load_hls(&self.hls_cache, spec)
    }

    pub(crate) fn parse_hls_spec(&self, encoded: &str) -> Result<ResolvedHlsSpec, HlsSpecError> {
        parse_hls_spec_with(encoded, |key| self.resolve_hls_blob(key))
    }

    pub(crate) fn register_hls_blob(&self, bytes: &[u8]) -> String {
        let key = crate::hls_blob_store::blob_key(bytes);
        let mut blobs = self.hls_blobs.write().expect("hls blob store poisoned");
        blobs
            .entry(key.clone())
            .or_insert_with(|| Arc::new(bytes.to_vec()));
        key
    }

    fn resolve_hls_blob(&self, key: &str) -> Result<Arc<Vec<u8>>, HlsSpecError> {
        let blobs = self.hls_blobs.read().expect("hls blob store poisoned");
        blobs
            .get(key)
            .cloned()
            .ok_or_else(|| HlsSpecError::MissingBlob(key.to_owned()))
    }

    pub(crate) fn resolve_hls_spec(&self, spec: HlsSpec) -> Result<ResolvedHlsSpec, HlsSpecError> {
        resolve_hls_spec_with(spec, |key| self.resolve_hls_blob(key))
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[kithara::test(native, flash(false))]
    fn behavior_register_returns_token_and_counts_start_at_zero() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::HtmlError("<html>captive</html>"),
            delivery: Delivery::Normal,
        });
        assert_eq!(state.behavior_hits(&token), Some(0));
        assert!(state.get_behavior(&token).is_some());
        assert_eq!(state.behavior_hits("nonexistent"), None);
    }

    #[kithara::test(native, flash(false))]
    fn behavior_bump_increments_count() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::Status(503),
            delivery: Delivery::Normal,
        });
        state.bump_behavior(&token);
        state.bump_behavior(&token);
        assert_eq!(state.behavior_hits(&token), Some(2));
    }
}
