use std::{
    collections::HashMap,
    sync::{
        RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use kithara::platform::{
    sync::{Arc, Notify},
    tokio::sync::watch,
};
use moka::sync::Cache;
use uuid::Uuid;

use crate::{
    hls_spec::{HlsSpecError, ResolvedHlsSpec, parse_hls_spec_with, resolve_hls_spec_with},
    hls_stream::{GeneratedHls, GeneratedHlsCache, load_hls},
    hls_url::HlsSpec,
    signal_spec::SignalRequest,
};

/// Soft cap on distinct encoded-signal entries kept in memory per
/// `TestServerState`. Each entry is a couple of `MiB` at most; 256
/// leaves plenty of headroom while bounding worst-case test-server
/// memory use.
const ENCODED_SIGNAL_CACHE_CAPACITY: u64 = 256;

#[derive(Clone)]
pub(crate) struct EncodedSignal {
    pub bytes: Arc<Vec<u8>>,
    pub content_type: &'static str,
}

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
    EarlyClose { after_bytes: usize },
    Throttle { chunk: usize, delay_ms: u64 },
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
///   synchronously at `Audio::new()`). The `head_requested` counter lets a test
///   observe the HEAD arrival.
///
/// Lives in `TestServerState` (mutable, per-token) — never in the immutable
/// Arc-cached `GeneratedHls`.
pub(crate) struct SegmentGate {
    released: watch::Sender<bool>,
    requested: AtomicU64,
    head_withheld: AtomicBool,
    head_requested: AtomicU64,
}

impl SegmentGate {
    fn new() -> Self {
        let (released, _rx) = watch::channel(false);
        Self {
            released,
            requested: AtomicU64::new(0),
            head_withheld: AtomicBool::new(false),
            head_requested: AtomicU64::new(0),
        }
    }

    pub(crate) fn mark_requested(&self) {
        self.requested.fetch_add(1, Ordering::Relaxed);
    }

    /// Park until [`Self::release`] is called. Returns immediately if already
    /// released — a fresh receiver observes the latest value first.
    pub(crate) async fn wait_until_released(&self) {
        let mut rx = self.released.subscribe();
        // `Err` only if the sender was dropped (gate removed) — proceed then.
        let _ = rx.wait_for(|released| *released).await;
    }

    /// Release the withheld segment so its GET response completes.
    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    /// In-process count of GET requests that reached this gate.
    pub(crate) fn requested(&self) -> u64 {
        self.requested.load(Ordering::Relaxed)
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
/// **blocking** construction read (`Audio::new`), whose first read of the
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
pub(crate) struct InitGate {
    released: watch::Sender<bool>,
    requested: AtomicU64,
}

impl InitGate {
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

    /// Park until [`Self::release`] is called. Returns immediately if already
    /// released — a fresh receiver observes the latest value first.
    pub(crate) async fn wait_until_released(&self) {
        let mut rx = self.released.subscribe();
        // `Err` only if the sender was dropped (gate removed) — proceed then.
        let _ = rx.wait_for(|released| *released).await;
    }

    /// Release the withheld init segment so its parked GET (body) completes.
    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    /// In-process count of init GET requests that reached this gate.
    pub(crate) fn requested(&self) -> u64 {
        self.requested.load(Ordering::Relaxed)
    }
}

fn init_gate_key(hls_token: &str, variant: usize) -> String {
    format!("{hls_token}|v{variant}|init")
}

/// A virtual-time withhold gate for one `(hls token, variant, segment)` whose
/// body delay is driven by a FLASH PARTICIPANT, not the server thread.
///
/// The shared test-server thread is a real-time island (no flash ambient), so a
/// `sleep(delay_ms)` there would burn real wall-clock and stay invisible to the
/// client's virtual clock — under flash the engine would jump past the client's
/// fetch without the slow variant ever registering on ABR's throughput estimate.
///
/// Instead the segment GET parks on `released` (like [`SegmentGate`]); the delay
/// is timed by a releaser task spawned from the test's flash-ambient context. The
/// releaser awaits the GET's arrival (`wait_requested`), then `sleep`s `delay_ms`
/// of VIRTUAL time (its flash region makes the platform `sleep` engine-backed),
/// then [`Self::release`]s the body. So the client's fetch-duration spans
/// `delay_ms` of virtual time — ABR sees the slow variant — while the server
/// thread consumes zero real wall-clock holding the socket open.
///
/// Lives in `TestServerState` (mutable, per-token) — never in the immutable
/// Arc-cached `GeneratedHls`.
pub(crate) struct DelayGate {
    released: watch::Sender<bool>,
    requested: AtomicU64,
    request_arrived: Notify,
}

impl DelayGate {
    fn new() -> Self {
        let (released, _rx) = watch::channel(false);
        Self {
            released,
            requested: AtomicU64::new(0),
            request_arrived: Notify::default(),
        }
    }

    /// Mark that a segment GET reached this gate and wake the releaser so its
    /// virtual countdown starts at the moment the fetch actually arrives.
    pub(crate) fn mark_requested(&self) {
        self.requested.fetch_add(1, Ordering::Relaxed);
        self.request_arrived.notify_one();
    }

    /// Park until [`Self::release`] is called. Returns immediately if already
    /// released — a fresh receiver observes the latest value first.
    pub(crate) async fn wait_until_released(&self) {
        let mut rx = self.released.subscribe();
        // `Err` only if the sender was dropped (gate removed) — proceed then.
        let _ = rx.wait_for(|released| *released).await;
    }

    /// Park until the gated segment GET has reached the server at least once.
    /// Returns immediately if a request already arrived before the releaser
    /// began waiting (the permit from `notify_one` is stored).
    pub(crate) async fn wait_requested(&self) {
        if self.requested.load(Ordering::Relaxed) > 0 {
            return;
        }
        self.request_arrived.notified().await;
    }

    /// Release the withheld segment so its parked GET (body) response completes.
    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }
}

fn delay_gate_key(hls_token: &str, variant: usize, segment: usize) -> String {
    format!("{hls_token}|v{variant}|s{segment}|delay")
}

pub(crate) struct TestServerState {
    hls_cache: GeneratedHlsCache,
    hls_blobs: RwLock<HashMap<String, Arc<Vec<u8>>>>,
    tokens: RwLock<HashMap<String, StoredToken>>,
    encoded_signals: Cache<String, EncodedSignal>,
    behaviors: RwLock<HashMap<String, Arc<BehaviorEntry>>>,
    segment_gates: RwLock<HashMap<String, Arc<SegmentGate>>>,
    init_gates: RwLock<HashMap<String, Arc<InitGate>>>,
    delay_gates: RwLock<HashMap<String, Arc<DelayGate>>>,
    /// Per-`(hls token, variant, segment)` count of size-probe requests the
    /// server has served: every `HEAD` and every single-byte ranged
    /// `GET` (`Range: bytes=0-0`). Unlike the withhold gates this counter is
    /// always live (no pre-registration), so a test can observe the up-front
    /// size-estimation pass that probes every segment of every variant at
    /// `Audio::new()` versus the lazy per-segment resolve that probes only
    /// the active prefix.
    size_probes: RwLock<HashMap<String, AtomicU64>>,
}

#[derive(Clone)]
enum StoredToken {
    Signal(SignalRequest),
    Hls(Arc<GeneratedHls>),
}

impl TestServerState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: RwLock::new(HashMap::new()),
            hls_cache: RwLock::new(HashMap::new()),
            hls_blobs: RwLock::new(HashMap::new()),
            encoded_signals: Cache::new(ENCODED_SIGNAL_CACHE_CAPACITY),
            behaviors: RwLock::new(HashMap::new()),
            segment_gates: RwLock::new(HashMap::new()),
            init_gates: RwLock::new(HashMap::new()),
            delay_gates: RwLock::new(HashMap::new()),
            size_probes: RwLock::new(HashMap::new()),
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

    /// Register a withhold gate for one `(hls token, variant, segment)` and
    /// return its handle. The matching segment GET parks until [`SegmentGate::release`].
    pub(crate) fn register_segment_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> Arc<SegmentGate> {
        let gate = Arc::new(SegmentGate::new());
        let mut map = self.segment_gates.write().expect("segment gates poisoned");
        map.insert(
            segment_gate_key(hls_token, variant, segment),
            Arc::clone(&gate),
        );
        gate
    }

    pub(crate) fn segment_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> Option<Arc<SegmentGate>> {
        let map = self.segment_gates.read().expect("segment gates poisoned");
        map.get(&segment_gate_key(hls_token, variant, segment))
            .map(Arc::clone)
    }

    /// Register a withhold gate for one `(hls token, variant)` init
    /// (`EXT-X-MAP`) segment and return its handle. The matching init GET
    /// parks until [`InitGate::release`], holding the owning track's loader in
    /// `Loading`.
    pub(crate) fn register_init_gate(&self, hls_token: &str, variant: usize) -> Arc<InitGate> {
        let gate = Arc::new(InitGate::new());
        let mut map = self.init_gates.write().expect("init gates poisoned");
        map.insert(init_gate_key(hls_token, variant), Arc::clone(&gate));
        gate
    }

    pub(crate) fn init_gate(&self, hls_token: &str, variant: usize) -> Option<Arc<InitGate>> {
        let map = self.init_gates.read().expect("init gates poisoned");
        map.get(&init_gate_key(hls_token, variant)).map(Arc::clone)
    }

    /// Register a virtual-time delay gate for one `(hls token, variant, segment)`
    /// and return its handle. The matching segment GET parks on the gate until a
    /// flash-participant releaser fires `delay_ms` of virtual time after the GET
    /// arrives (see [`DelayGate`]).
    pub(crate) fn register_delay_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> Arc<DelayGate> {
        let gate = Arc::new(DelayGate::new());
        let mut map = self.delay_gates.write().expect("delay gates poisoned");
        map.insert(
            delay_gate_key(hls_token, variant, segment),
            Arc::clone(&gate),
        );
        gate
    }

    pub(crate) fn delay_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> Option<Arc<DelayGate>> {
        let map = self.delay_gates.read().expect("delay gates poisoned");
        map.get(&delay_gate_key(hls_token, variant, segment))
            .map(Arc::clone)
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

    /// Lookup a previously encoded signal payload. Test fixture builders
    /// (`TestServerHelper::{sine,sweep,…}`) populate this cache at setup
    /// time via [`Self::insert_encoded_signal`], so request handlers
    /// never encode on the critical path (the production analogue is "a
    /// file already exists on disk"). A miss is a fixture-setup bug.
    pub(crate) fn get_encoded_signal(&self, key: &str) -> Option<EncodedSignal> {
        self.encoded_signals.get(key)
    }

    /// Insert a pre-encoded signal payload. Test helpers call this at
    /// fixture build time so that the request handler can serve range
    /// requests immediately, without inline encoding work.
    pub(crate) fn insert_encoded_signal(&self, key: String, encoded: EncodedSignal) {
        self.encoded_signals.insert(key, encoded);
    }

    pub(crate) fn get_hls(&self, token: &str) -> Option<Arc<GeneratedHls>> {
        let store = self.tokens.read().expect("token store poisoned");
        match store.get(token) {
            Some(StoredToken::Hls(hls)) => Some(Arc::clone(hls)),
            _ => None,
        }
    }

    pub(crate) fn get_signal(&self, token: &str) -> Option<SignalRequest> {
        let store = self.tokens.read().expect("token store poisoned");
        match store.get(token) {
            Some(StoredToken::Signal(request)) => Some(request.clone()),
            _ => None,
        }
    }

    fn insert(&self, value: StoredToken) -> String {
        let token = Uuid::new_v4().to_string();
        let mut store = self.tokens.write().expect("token store poisoned");
        store.insert(token.clone(), value);
        token
    }

    fn insert_hls(&self, spec: ResolvedHlsSpec) -> Result<String, HlsSpecError> {
        let hls = self.load_hls(spec)?;
        Ok(self.insert(StoredToken::Hls(hls)))
    }

    pub(crate) fn insert_hls_spec(&self, spec: HlsSpec) -> Result<String, HlsSpecError> {
        let resolved = self.resolve_hls_spec(spec)?;
        self.insert_hls(resolved)
    }

    pub(crate) fn insert_signal(&self, request: SignalRequest) -> String {
        self.insert(StoredToken::Signal(request))
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

    #[test]
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

    #[test]
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
