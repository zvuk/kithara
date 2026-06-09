use std::{env, sync::Arc};

use axum::{Router, routing::get};
use kithara_platform::{
    time::{Duration, flash_dynamic, sleep},
    tokio::task::spawn,
};
use tower_http::cors::CorsLayer;
use tracing::trace;
use url::Url;

use crate::{
    fixture_protocol::DelayRule,
    hls_url::HlsSpec,
    http_server::TestHttpServer,
    routes::{
        assets, behavior, signal,
        signal::{encode_signal_payload, encoded_signal_cache_key},
        stream,
    },
    signal_spec::{SignalKind as InternalSignalKind, parse_signal_request},
    signal_url::{SignalKind, SignalSpec, signal_path},
    test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder},
    test_server_state::{DelayGate, FixtureBehavior, InitGate, SegmentGate, TestServerState},
};

/// Facade over the process-global shared test server.
pub struct TestServerHelper {
    state: Arc<TestServerState>,
    base_url: Url,
}

impl TestServerHelper {
    /// Borrow the process-global server's state and base URL.
    pub async fn new() -> Self {
        let shared = crate::test_server::shared();
        Self {
            state: Arc::clone(&shared.state),
            base_url: shared.base_url.clone(),
        }
    }

    /// Build a URL for a static test asset.
    #[must_use]
    pub fn asset(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.url(&format!("/assets/{trimmed}"))
    }

    /// Build a URL for the static asset `name` exposed via a path with no
    /// file extension — `/streamhq?name=...`. Mirrors the production
    /// `cdn-edge.zvq.me/track/streamhq?id=*` shape so tests can pin that
    /// the decoder doesn't rely on extension-based mime sniffing.
    #[must_use]
    pub fn streamhq(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.url(&format!("/streamhq?name={trimmed}"))
    }

    /// Base URL of this server.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Register an HLS fixture from a builder, storing media blobs in the server.
    ///
    /// # Errors
    ///
    /// Returns [`CreateHlsError`] if inserting the resolved spec fails.
    pub async fn create_hls(
        &self,
        builder: HlsFixtureBuilder,
    ) -> Result<CreatedHls, CreateHlsError> {
        let spec =
            builder.into_spec_with_blob_registrar(|bytes| self.state.register_hls_blob(bytes));
        self.create_hls_from_spec(spec).await
    }

    pub(crate) async fn create_hls_from_spec(
        &self,
        spec: HlsSpec,
    ) -> Result<CreatedHls, CreateHlsError> {
        let token = self.state.insert_hls_spec(spec)?;
        Ok(CreatedHls::new(self.base_url().clone(), token))
    }

    /// Build a URL for `/signal/sawtooth/...`.
    #[must_use]
    pub async fn sawtooth(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::Sawtooth, spec).await
    }

    /// Build a URL for `/signal/sawtooth-desc/...`.
    #[must_use]
    pub async fn sawtooth_descending(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::SawtoothDescending, spec).await
    }

    async fn signal_url(&self, kind: SignalKind, spec: &SignalSpec) -> Url {
        let path = signal_path(kind, spec);
        let prefix = format!("/signal/{}/", kind.path_segment());
        let spec_with_ext = path
            .strip_prefix(&prefix)
            .expect("signal path must match kind prefix");
        let internal_kind =
            InternalSignalKind::try_from(kind.path_segment()).expect("valid signal route");
        let request =
            parse_signal_request(internal_kind, spec_with_ext).expect("valid signal spec");
        let token = self.state.insert_signal(request.clone());
        let token_with_ext = format!("{token}.{}", spec.format.path_ext());

        // Encode at fixture build time so the request handler can serve
        // range requests with no inline work. Mirrors the production
        // analogue where a media file already exists on disk by the time
        // the player opens it. Stay in spawn_blocking to keep the
        // encoder off the tokio worker thread.
        let cache_key = encoded_signal_cache_key(internal_kind, &token_with_ext);
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            if let Some(encoded) = encode_signal_payload(&request) {
                state.insert_encoded_signal(cache_key, encoded);
            }
        })
        .await
        .expect("signal pre-encode task panicked");

        self.url(&format!("/signal/{}/{token_with_ext}", kind.path_segment(),))
    }

    /// Build a URL for `/signal/silence/...`.
    #[must_use]
    pub async fn silence(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::Silence, spec).await
    }

    /// Build a URL for `/signal/sine/...`.
    #[must_use]
    pub async fn sine(&self, spec: &SignalSpec, freq_hz: f64) -> Url {
        self.signal_url(SignalKind::Sine { freq_hz }, spec).await
    }

    /// Build an arbitrary URL on this server.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        self.base_url.join(path).expect("join server URL path")
    }

    /// Register a fixture behavior and return a handle exposing its URL and
    /// in-process request count.
    #[must_use]
    pub fn register_behavior(&self, behavior: FixtureBehavior) -> BehaviorHandle {
        let token = self.state.insert_behavior(behavior);
        BehaviorHandle {
            state: Arc::clone(&self.state),
            base_url: self.base_url.clone(),
            token,
        }
    }

    /// Register a withhold gate for one media segment of the fixture behind
    /// `hls_token`, returning a handle that releases it and reports how many
    /// segment GETs it has parked. The matching GET response is withheld until
    /// [`SegmentGateHandle::release`] — a deterministic, release-driven seam
    /// (no timers) for "this segment has not arrived yet" scenarios.
    #[must_use]
    pub fn register_segment_gate(
        &self,
        hls_token: &str,
        variant: usize,
        segment: usize,
    ) -> SegmentGateHandle {
        let gate = self
            .state
            .register_segment_gate(hls_token, variant, segment);
        SegmentGateHandle { gate }
    }

    /// Register a withhold gate for the init (`EXT-X-MAP`) segment of one
    /// variant of the fixture behind `hls_token`, returning a handle that
    /// releases it and reports how many init GETs it has parked. The matching
    /// init GET response is withheld until [`InitGateHandle::release`].
    ///
    /// The off-RT blocking construction read (`Audio::new`, inside
    /// `Resource::new`) reads the init body, so a held init gate parks that read
    /// and keeps the owning track's loader in `TrackStatus::Loading` — a
    /// release-driven lever (no timers, no wall-clock segment delays) for "this
    /// track is still constructing" scenarios.
    #[must_use]
    pub fn register_init_gate(&self, hls_token: &str, variant: usize) -> InitGateHandle {
        let gate = self.state.register_init_gate(hls_token, variant);
        InitGateHandle { gate }
    }

    /// Arm a virtual-time delay gate for every `(variant, segment)` of `hls_token`
    /// whose `delay_rules` resolve to a non-zero delay, and spawn its flash-aware
    /// releaser. Called from the test's flash-ambient setup (`HlsTestServer::new`),
    /// so each releaser inherits `FLASH_AMBIENT` via the platform async [`spawn`]
    /// chokepoint and its `sleep` runs on VIRTUAL time — the slow-variant delay
    /// the client observes is engine-backed, not real wall-clock.
    pub fn arm_delay_gates(
        &self,
        hls_token: &str,
        variant_count: usize,
        segments_per_variant: usize,
        delay_rules: &[DelayRule],
    ) {
        if delay_rules.is_empty() {
            return;
        }
        for variant in 0..variant_count {
            for segment in 0..segments_per_variant {
                let Some(delay_ms) = delay_rules
                    .iter()
                    .find_map(|rule| rule.matches(variant, segment))
                    .filter(|&ms| ms > 0)
                else {
                    continue;
                };
                let gate = self.state.register_delay_gate(hls_token, variant, segment);
                spawn_delay_releaser(gate, delay_ms, variant, segment);
            }
        }
    }
}

/// Spawn the flash-participant releaser for one delay gate. The body runs inside a
/// `flash_dynamic(true, ...)` region (which only takes effect under ambient, which
/// the platform async [`spawn`] propagates), so its `sleep` is engine-backed: it
/// awaits the segment GET's arrival, burns `delay_ms` of VIRTUAL time, then frees
/// the parked body. Off the `flash-time` feature `flash_dynamic` is identity and
/// the `sleep` is a real `tokio` timer — matching the legacy real-delay behaviour.
fn spawn_delay_releaser(gate: Arc<DelayGate>, delay_ms: u64, variant: usize, segment: usize) {
    drop(spawn(flash_dynamic(true, async move {
        gate.wait_requested().await;
        trace!(
            variant,
            segment, delay_ms, "delay gate: request arrived, starting virtual countdown"
        );
        sleep(Duration::from_millis(delay_ms)).await;
        gate.release();
        trace!(
            variant,
            segment, delay_ms, "delay gate: released after virtual delay"
        );
    })));
}

/// Handle to a registered init-segment withhold gate on the shared server.
#[derive(Clone)]
pub struct InitGateHandle {
    gate: Arc<InitGate>,
}

impl InitGateHandle {
    /// Release the withheld init segment so its parked GET (body) response
    /// completes, letting `Hls::create` (and the owning track's loader) proceed.
    pub fn release(&self) {
        self.gate.release();
    }

    /// Number of init GET (body) requests this gate has parked, observed
    /// in-process.
    #[must_use]
    pub fn requested(&self) -> u64 {
        self.gate.requested()
    }
}

/// Handle to a registered segment withhold gate on the shared server.
#[derive(Clone)]
pub struct SegmentGateHandle {
    gate: Arc<SegmentGate>,
}

impl SegmentGateHandle {
    /// Release the withheld segment so its parked GET (body) response completes.
    pub fn release(&self) {
        self.gate.release();
    }

    /// Number of segment GET (body) requests this gate has parked, observed
    /// in-process.
    #[must_use]
    pub fn requested(&self) -> u64 {
        self.gate.requested()
    }

    /// Withhold the segment's size: subsequent HEAD (size) requests report
    /// `Content-Length: 0`, so the up-front size-estimation pass learns a zero
    /// size for it. Models "seek before this segment's size is known".
    /// Independent of the body [`Self::release`] withhold.
    pub fn withhold_head(&self) {
        self.gate.withhold_head();
    }

    /// Reveal the true size on subsequent HEAD (size) requests.
    pub fn release_head(&self) {
        self.gate.release_head();
    }

    /// Number of HEAD (size) requests this gate has observed in-process.
    #[must_use]
    pub fn head_requested(&self) -> u64 {
        self.gate.head_requested()
    }
}

/// Handle to a registered fixture behavior on the shared server.
#[derive(Clone)]
pub struct BehaviorHandle {
    state: Arc<TestServerState>,
    base_url: Url,
    token: String,
}

impl BehaviorHandle {
    /// URL that dispatches to this behavior.
    #[must_use]
    pub fn url(&self) -> Url {
        self.base_url
            .join(&format!("/behavior/{}", self.token))
            .expect("join behavior url")
    }

    /// URL on the same fixture with an arbitrary trailing path (e.g. to give the
    /// decoder a file-extension hint via the last path segment).
    #[must_use]
    pub fn child_url(&self, rest: &str) -> Url {
        self.base_url
            .join(&format!(
                "/behavior/{}/{}",
                self.token,
                rest.trim_start_matches('/')
            ))
            .expect("join behavior child url")
    }

    /// Number of requests this behavior has served, observed in-process.
    #[must_use]
    pub fn request_count(&self) -> u64 {
        self.state.behavior_hits(&self.token).unwrap_or(0)
    }
}

async fn health() -> &'static str {
    "ok"
}

/// Start the server as a standalone process (used by the `test_server` binary).
pub async fn run_test_server() {
    let port: u16 = env::var("TEST_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3444);
    let state = TestServerState::new();
    let mut server = TestHttpServer::bind(&format!("127.0.0.1:{port}"), router(state)).await;
    println!("test server listening on {}", server.base_url());
    server.completion().await;
}

pub(crate) fn router(state: Arc<TestServerState>) -> Router {
    Router::<Arc<TestServerState>>::new()
        .route("/health", get(health))
        .merge(assets::router())
        .merge(behavior::router())
        .merge(signal::router())
        .merge(stream::router())
        .merge(crate::routes::token::router())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kithara,
        test_server_state::{Content, Delivery},
    };

    #[kithara::test(tokio)]
    async fn two_helpers_share_one_base_url() {
        let a = TestServerHelper::new().await;
        let b = TestServerHelper::new().await;
        assert_eq!(
            a.base_url(),
            b.base_url(),
            "all helpers reuse the shared server"
        );
    }

    #[kithara::test(tokio)]
    async fn behavior_handle_reports_in_process_count() {
        let helper = TestServerHelper::new().await;
        let handle = helper.register_behavior(FixtureBehavior {
            content: Content::Status(404),
            delivery: Delivery::Normal,
        });
        assert_eq!(handle.request_count(), 0);
        let _ = reqwest::get(handle.url()).await.unwrap();
        assert_eq!(handle.request_count(), 1);
    }
}
