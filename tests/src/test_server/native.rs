use std::env;

use axum::{Router, middleware, routing::get};
use kithara::platform::sync::Arc;
use kithara_test_fixtures::{Mp3Shape, SignalAsset, assets::by_name};
use tower_http::cors::CorsLayer;
use url::Url;

use crate::{
    hls_url::HlsSpec,
    http_server::TestHttpServer,
    routes::{assets, behavior, control, signal, store, stream},
    test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder},
    test_server_state::{
        Content, Delivery, FixtureBehavior, InitGate, NetworkMode, SegmentGate, TestServerState,
    },
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

    pub(crate) const fn for_server(state: Arc<TestServerState>, base_url: Url) -> Self {
        Self { state, base_url }
    }

    /// Build a URL for a static test asset.
    #[must_use]
    pub fn asset(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.url(&format!("/assets/{trimmed}"))
    }

    /// Build a URL for one generated body exposed via a path with no file
    /// extension — `/streamhq?name=...`. Mirrors the production
    /// `cdn-edge.zvq.me/track/streamhq?id=*` shape so tests can pin that
    /// the decoder doesn't rely on extension-based mime sniffing.
    #[must_use]
    pub fn streamhq(&self, asset: SignalAsset) -> Url {
        self.url(&format!("/streamhq?name={}.{}", asset.name(), asset.ext()))
    }

    /// Base URL of this server.
    #[must_use]
    pub const fn base_url(&self) -> &Url {
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

    /// URL of one build-time generated signal body.
    #[must_use]
    pub fn signal(&self, asset: SignalAsset) -> Url {
        self.url(&asset.path())
    }

    /// URL of one build-time generated signal body, served in `shape`.
    ///
    /// The generator always writes a Xing/Info frame; real CBR content often
    /// has none, which leaves its byte length as the only record of duration.
    /// The stored body is the tagged shape, so any other shape is reshaped
    /// here and served through a registered behavior over the same
    /// range-capable route.
    #[must_use]
    pub fn signal_in(&self, asset: SignalAsset, shape: Mp3Shape) -> Url {
        if shape == Mp3Shape::Tagged {
            return self.signal(asset);
        }
        let generated = by_name(asset.name()).expect("asset name comes from the generator");
        let handle = self.register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(shape.apply(generated.bytes())),
                content_type: Some(generated.entry().content_type),
            },
            delivery: Delivery::Range,
        });
        handle.child_url(&format!("{}.{}", asset.name(), asset.ext()))
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

    /// Size-probes (`HEAD` + single-byte ranged `GET`) the server has served
    /// for one `(hls token, variant, segment)`. Always live (no gate needed),
    /// so a test can observe the up-front size-estimation storm at
    /// `PlayWorker::open` versus the lazy per-segment resolve.
    #[must_use]
    pub fn size_probe_count(&self, hls_token: &str, variant: usize, segment: usize) -> u64 {
        self.state.size_probe_count(hls_token, variant, segment)
    }

    /// Register a withhold gate for the init (`EXT-X-MAP`) segment of one
    /// variant of the fixture behind `hls_token`, returning a handle that
    /// releases it and reports how many init GETs it has parked. The matching
    /// init GET response is withheld until [`InitGateHandle::release`].
    ///
    /// The off-RT blocking preparation read (`PlayWorker::open`, inside
    /// `Resource::new`) reads the init body, so a held init gate parks that read
    /// and keeps the owning track's loader in `TrackStatus::Loading` — a
    /// release-driven lever (no timers, no wall-clock segment delays) for "this
    /// track is still constructing" scenarios.
    #[must_use]
    pub fn register_init_gate(&self, hls_token: &str, variant: usize) -> InitGateHandle {
        let gate = self.state.register_init_gate(hls_token, variant);
        InitGateHandle { gate }
    }
}

/// A test server private to one test: its own port, its own [`TestServerState`].
///
/// Reachability is server-wide — the guard in front of every data route reads
/// one flag — so a test that takes the network down must not share a server with
/// anything else. On the process-global server ([`TestServerHelper::new`]) that
/// outage answers every parallel sibling's request with `503`, and the siblings
/// fail on preconditions that have nothing to do with what they assert.
///
/// Owning the switch here rather than on [`TestServerHelper`] is what keeps that
/// from coming back: the shared helper has no way to take a server offline.
///
/// Dropping this shuts its server down.
pub struct PrivateTestServer {
    state: Arc<TestServerState>,
    server: TestHttpServer,
}

impl PrivateTestServer {
    /// Start a server private to this test on its own loopback port.
    pub async fn start() -> Self {
        let state = TestServerState::new();
        let server = TestHttpServer::new(router(Arc::clone(&state))).await;
        Self { state, server }
    }

    /// A helper bound to this private server instead of the shared one.
    #[must_use]
    pub fn helper(&self) -> TestServerHelper {
        TestServerHelper::for_server(Arc::clone(&self.state), self.server.base_url().clone())
    }

    /// Lower or raise this server's reachability switch.
    ///
    /// In-process counterpart of `POST /control/network`.
    pub fn set_network_mode(&self, mode: NetworkMode) {
        self.state.set_network_mode(mode);
    }
}

/// Handle to a registered init-segment withhold gate on the shared server.
#[derive(Clone)]
pub struct InitGateHandle {
    gate: Arc<InitGate>,
}

impl InitGateHandle {
    delegate::delegate! {
        to self.gate {
            /// Release the withheld init segment so its parked GET (body) response
            /// completes, letting `Hls::create` (and the owning track's loader) proceed.
            pub fn release(&self);
            /// Number of init GET (body) requests this gate has parked, observed
            /// in-process.
            #[must_use]
            pub fn requested(&self) -> u64;
        }
    }
}

/// Handle to a registered segment withhold gate on the shared server.
#[derive(Clone)]
pub struct SegmentGateHandle {
    gate: Arc<SegmentGate>,
}

impl SegmentGateHandle {
    delegate::delegate! {
        to self.gate {
            /// Release the withheld segment so its parked GET (body) response completes.
            pub fn release(&self);
            /// Number of segment GET (body) requests this gate has parked, observed
            /// in-process.
            #[must_use]
            pub fn requested(&self) -> u64;
            /// Withhold the segment's size: subsequent HEAD (size) requests report
            /// `Content-Length: 0`, so the up-front size-estimation pass learns a zero
            /// size for it. Models "seek before this segment's size is known".
            /// Independent of the body [`Self::release`] withhold.
            pub fn withhold_head(&self);
            /// Reveal the true size on subsequent HEAD (size) requests.
            pub fn release_head(&self);
            /// Number of HEAD (size) requests this gate has observed in-process.
            #[must_use]
            pub fn head_requested(&self) -> u64;
        }
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

/// First stdout line of the standalone server, followed by the base URL it
/// bound. The harness that spawns the process reads the address from here, so
/// the wording is a contract with `xtask`: `TEST_SERVER_PORT=0` asks the OS for
/// a port, and this line is the only place the answer is published.
const STARTUP_RECORD: &str = "test server listening on";

/// Start the server as a standalone process (used by the `test_server` binary).
pub async fn run_test_server() {
    let port: u16 = env::var("TEST_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3444);
    let state = TestServerState::new();
    let routes = router(state);
    #[cfg(not(target_os = "android"))]
    let routes = routes.merge(crate::pcm_oracle::router());
    let mut server = TestHttpServer::bind(&format!("127.0.0.1:{port}"), routes).await;
    println!("{STARTUP_RECORD} {}", server.base_url());
    server.completion().await;
}

pub(crate) fn router(state: Arc<TestServerState>) -> Router {
    Router::<Arc<TestServerState>>::new()
        .route("/health", get(health))
        .merge(assets::router())
        .merge(behavior::router())
        .merge(signal::router())
        .merge(store::router())
        .merge(stream::router())
        .merge(crate::routes::token::router())
        .merge(control::router())
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            control::network_guard,
        ))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use kithara::platform::time::{Duration, Instant};

    use super::{Content, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper};
    use crate::{fixture_protocol::DelayRule, kithara};

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

    #[kithara::test(tokio, flash(false))]
    async fn repeated_segment_gets_each_observe_configured_delay() {
        check_repeated_segment_delay().await;
    }

    #[kithara::test(tokio, flash(true))]
    async fn repeated_segment_gets_each_observe_virtual_delay() {
        check_repeated_segment_delay().await;
    }

    #[kithara::flash(true)]
    async fn check_repeated_segment_delay() {
        const DELAY_MS: u64 = 250;
        let helper = TestServerHelper::new().await;
        let created = helper
            .create_hls(
                HlsFixtureBuilder::new()
                    .variant_count(2)
                    .segments_per_variant(2)
                    .push_delay_rule(DelayRule {
                        variant: Some(1),
                        segment_eq: Some(1),
                        delay_ms: DELAY_MS,
                        ..DelayRule::default()
                    }),
            )
            .await
            .unwrap();
        let delay = Duration::from_millis(DELAY_MS);
        let client = reqwest::Client::new();
        for request in 0..2 {
            let started = Instant::now();
            let response = client.get(created.segment_url(1, 1)).send().await.unwrap();
            assert!(response.status().is_success());
            assert!(!response.bytes().await.unwrap().is_empty());
            assert!(
                started.elapsed() >= delay,
                "GET {request} bypassed the configured response delay"
            );
        }
    }
}
