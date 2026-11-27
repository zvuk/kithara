use std::{convert::Infallible, fs, num::NonZeroU32, path::PathBuf, pin::pin};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use futures::stream;
use kithara_analysis::{AnalysisToken, TrackAnalysis};
use kithara_assets::{AssetResource, AssetResourceState, AssetStore, StorageBackend};
use kithara_download::{Downloader, DownloaderConfig};
use kithara_net::{Headers, HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, timeout},
    tokio::{self, runtime::Handle, sync::watch},
};
use kithara_test_utils::{TestHttpServer, TestTempDir, bufpool::pools, kithara, temp_dir};
use url::Url;

use super::{ArtifactDocument, ArtifactLoadError, ArtifactRequest, Cover, MAX_ARTIFACT_BYTES};
use crate::{
    resource::{ResourceConfig, ResourceSrc},
    test_pools::TestPools,
};

fn downloader() -> Downloader {
    let client = HttpClient::new(NetOptions::default(), pools(), CancelToken::never());
    Downloader::new(
        DownloaderConfig::for_client(client)
            .runtime(Handle::current())
            .build(),
    )
}

fn memory_store() -> AssetStore<TestPools> {
    AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .build()
}

fn disk_store(dir: &TestTempDir) -> AssetStore<TestPools> {
    AssetStore::builder(pools())
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .build()
}

fn audio() -> ResourceSrc {
    ResourceSrc::Url(Url::parse("https://audio.example/track.mp3").expect("BUG: valid URL"))
}

fn audio_headers() -> Headers {
    let mut headers = Headers::default();
    headers.insert("Authorization", "Bearer audio-token");
    headers
}

/// A track whose audio is read with its own credentials.
fn track(audio: ResourceSrc, store: AssetStore<TestPools>) -> ResourceConfig<TestPools> {
    ResourceConfig::for_src(audio)
        .store(store)
        .downloader(downloader())
        .headers(audio_headers())
        .build()
}

fn request(url: Url) -> ArtifactRequest {
    ArtifactRequest {
        src: ResourceSrc::Url(url),
        headers: None,
    }
}

fn analysis() -> TrackAnalysis {
    TrackAnalysis::builder()
        .token(AnalysisToken::from("served"))
        .source_sample_rate(NonZeroU32::new(44_100).expect("BUG: non-zero rate"))
        .media_origin(Duration::ZERO)
        .extent(44_100)
        .settled(true)
        .revision(3)
        .build()
}

fn document() -> Vec<u8> {
    let mut bytes = Vec::new();
    analysis()
        .write_to(&mut bytes)
        .expect("BUG: a small snapshot encodes");
    bytes
}

/// A source the tests point an artifact at, written into the scratch
/// directory the harness owns.
fn file(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kithara-artifact-{name}"));
    fs::write(&path, bytes).expect("BUG: scratch write");
    path
}

/// Every request a [`Served`] document received, with its headers.
#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<HeaderMap>>>);

impl Requests {
    fn count(&self) -> usize {
        self.0.lock().len()
    }

    fn headers(&self, index: usize) -> HeaderMap {
        self.0.lock()[index].clone()
    }
}

/// A one-way signal the tests and the served routes give each other.
#[derive(Clone)]
struct Latch(watch::Sender<bool>);

impl Latch {
    fn new() -> Self {
        Self(watch::channel(false).0)
    }

    fn open(&self) {
        self.0.send_replace(true);
    }

    async fn opened(&self) {
        let mut rx = self.0.subscribe();
        assert!(
            rx.wait_for(|open| *open).await.is_ok(),
            "BUG: the latch outlives its waiters"
        );
    }
}

/// One document served at `/doc` for as long as the server lives.
struct Served {
    server: TestHttpServer,
    requests: Requests,
    /// Opens once the first request is in.
    arrived: Latch,
    /// Lets a held first request answer.
    release: Latch,
}

impl Served {
    async fn new(body: Vec<u8>) -> Self {
        Self::serve(body, StatusCode::OK, false).await
    }

    /// Serve `body`, holding the first request until [`Self::release`].
    async fn holding_first(body: Vec<u8>) -> Self {
        Self::serve(body, StatusCode::OK, true).await
    }

    async fn serve(body: Vec<u8>, status: StatusCode, hold_first: bool) -> Self {
        let requests = Requests::default();
        let (arrived, release) = (Latch::new(), Latch::new());
        let router = {
            let (requests, arrived, release) = (requests.clone(), arrived.clone(), release.clone());
            let answer = move |headers: HeaderMap| {
                let (requests, arrived, release, body) = (
                    requests.clone(),
                    arrived.clone(),
                    release.clone(),
                    body.clone(),
                );
                async move {
                    let first = {
                        let mut seen = requests.0.lock();
                        seen.push(headers);
                        seen.len() == 1
                    };
                    if first {
                        arrived.open();
                        if hold_first {
                            release.opened().await;
                        }
                    }
                    (status, body).into_response()
                }
            };
            Router::new()
                .route("/doc", get(answer.clone()))
                .route("/other", get(answer))
        };
        Self {
            server: TestHttpServer::new(router).await,
            requests,
            arrived,
            release,
        }
    }

    fn url(&self) -> Url {
        self.server.url("/doc")
    }

    /// Wait until the first request is in.
    async fn arrived(&self) {
        self.arrived.opened().await;
    }
}

/// A document is read whole on its own terms: a snapshot an analysis server
/// wrote decodes as the analysis it holds.
#[kithara::test]
fn an_analysis_document_decodes_the_snapshot_it_holds() {
    let read = <TrackAnalysis as ArtifactDocument>::decode(document())
        .expect("BUG: a written snapshot reads");

    assert_eq!(read.token().as_str(), "served");
    assert_eq!(read.revision(), 3);
    assert!(
        read.waveform().is_none(),
        "a document without a waveform has none"
    );
}

#[kithara::test]
fn an_analysis_document_of_a_foreign_version_is_refused() {
    let mut bytes = document();
    bytes[..4].copy_from_slice(&0x4b41_00ff_u32.to_le_bytes());

    let error = <TrackAnalysis as ArtifactDocument>::decode(bytes)
        .expect_err("BUG: another version is not this build's document");

    assert!(error.contains("version"), "{error}");
}

/// A request names its own credentials. The audio's never go with it, even
/// though the track is read with them.
#[kithara::test(tokio)]
async fn a_request_on_another_host_is_downloaded_with_its_own_headers_only() {
    let served = Served::new(document()).await;
    let config = track(audio(), memory_store());
    let mut headers = Headers::default();
    headers.insert("X-Analysis", "request-token");
    let request = ArtifactRequest {
        src: ResourceSrc::Url(served.url()),
        headers: Some(headers),
    };

    config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request, &CancelToken::never().child())
        .await
        .expect("BUG: a served document reads");

    let sent = served.requests.headers(0);
    assert_eq!(
        sent.get("x-analysis").and_then(|value| value.to_str().ok()),
        Some("request-token")
    );
    assert!(
        sent.get("authorization").is_none(),
        "the audio's token leaked"
    );
}

/// A request that names no headers sends none, even to the audio's own host.
#[kithara::test(tokio)]
async fn a_request_without_headers_is_downloaded_without_them() {
    let served = Served::new(document()).await;
    let audio = ResourceSrc::Url(served.server.url("/track.mp3"));
    let config = track(audio, memory_store());

    config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request(served.url()), &CancelToken::never().child())
        .await
        .expect("BUG: a served document reads");

    assert!(
        served.requests.headers(0).get("authorization").is_none(),
        "the audio's token went with a request that named no headers"
    );
}

/// The cover keeps the audio-host rule: the audio's credentials follow it to
/// the audio host only.
#[kithara::test(tokio)]
async fn a_cover_sends_the_audio_credentials_to_the_audio_host_only() {
    let served = Served::new(b"cover".to_vec()).await;
    let config = track(audio(), memory_store());
    let fetch = config.artifact_fetch();

    assert!(
        fetch.headers_for(&served.url()).is_none(),
        "a third host gets no audio credentials"
    );
    assert!(
        fetch
            .headers_for(&Url::parse("https://audio.example/cover.jpg").expect("BUG: valid URL"))
            .is_some(),
        "the audio host keeps its own"
    );
    let cover = fetch
        .load::<Cover>(&ResourceSrc::Url(served.url()))
        .await
        .expect("BUG: a served cover reads");
    assert_eq!(Vec::from(cover), b"cover");
    assert!(served.requests.headers(0).get("authorization").is_none());
}

#[kithara::test(tokio)]
async fn a_requested_path_is_read_from_its_file() {
    let path = file("analysis.bin", &document());
    let config = track(audio(), memory_store());
    let request = ArtifactRequest {
        src: ResourceSrc::Path(path),
        headers: None,
    };

    let read = config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request, &CancelToken::never().child())
        .await
        .expect("BUG: a written document reads");

    assert_eq!(read.revision(), 3);
}

/// A document the store already holds needs no network.
/// Store `bytes` as the document `url` names in `config`'s track scope.
fn store_document(config: &ResourceConfig<TestPools>, url: &Url, bytes: &[u8]) {
    let key = config
        .asset_key(&AssetResource::Url(url.clone()))
        .expect("BUG: the request URL keys inside the track's scope");
    let kithara_assets::AcquisitionResult::Pending(writer) = config
        .store()
        .acquire_resource(&key, None)
        .expect("BUG: an empty store acquires")
    else {
        panic!("an empty store hands out the writer");
    };
    kithara_assets::WriteSide::write_at(&writer, 0, bytes).expect("BUG: the document writes");
    drop(
        kithara_assets::WriteSide::commit(writer, Some(bytes.len() as u64))
            .expect("BUG: the document commits"),
    );
}

#[kithara::test(native, tokio)]
async fn a_stored_document_is_read_without_a_request(temp_dir: TestTempDir) {
    let served = Served::new(document()).await;
    let config = track(audio(), disk_store(&temp_dir));
    store_document(&config, &served.url(), &document());

    let read = config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request(served.url()), &CancelToken::never().child())
        .await
        .expect("BUG: a stored document reads");

    assert_eq!(read.revision(), 3);
    assert_eq!(
        served.requests.count(),
        0,
        "a stored document went to the network"
    );
}

/// A missing document is downloaded once and stored, and a restart finds it.
#[kithara::test(native, tokio)]
async fn a_missing_document_is_downloaded_once_and_outlives_a_restart(temp_dir: TestTempDir) {
    let served = Served::new(document()).await;
    let read = || async {
        let config = track(audio(), disk_store(&temp_dir));
        config
            .artifact_fetch()
            .read::<TrackAnalysis>(&request(served.url()), &CancelToken::never().child())
            .await
    };

    let first = read().await.expect("BUG: a served document reads");
    let again = read().await.expect("BUG: the stored document reads");

    assert_eq!(first.revision(), again.revision());
    assert_eq!(
        served.requests.count(),
        1,
        "the reopened store downloaded again"
    );
}

/// Two reads of one request at once make one download, and both decode. The
/// second read acquires on its first poll, while the first read's download
/// still holds the writer, so it joins that download.
#[kithara::test(native, tokio)]
async fn concurrent_reads_of_one_request_share_one_download(temp_dir: TestTempDir) {
    let served = Served::holding_first(document()).await;
    let config = track(audio(), disk_store(&temp_dir));
    let request = request(served.url());
    let cancel = CancelToken::never().child();
    let fetch = config.artifact_fetch();

    let (first, second) = tokio::join!(fetch.read::<TrackAnalysis>(&request, &cancel), async {
        served.arrived().await;
        let mut joined = pin!(fetch.read::<TrackAnalysis>(&request, &cancel));
        assert!(
            futures::poll!(joined.as_mut()).is_pending(),
            "the second read joins the download in flight"
        );
        served.release.open();
        joined.await
    });

    assert!(first.is_ok() && second.is_ok(), "{first:?} {second:?}");
    assert_eq!(served.requests.count(), 1);
}

/// A read that joined another read's download, which is then cancelled,
/// acquires again, downloads itself and decodes.
#[kithara::test(native, tokio)]
async fn a_read_joining_a_cancelled_download_acquires_again(temp_dir: TestTempDir) {
    let served = Served::holding_first(document()).await;
    let config = track(audio(), disk_store(&temp_dir));
    let request = request(served.url());
    let cancelled = CancelToken::never().child();
    let fetch = config.artifact_fetch();

    let (first, second) = tokio::join!(fetch.read::<TrackAnalysis>(&request, &cancelled), async {
        served.arrived().await;
        let own = CancelToken::never().child();
        let mut joined = pin!(fetch.read::<TrackAnalysis>(&request, &own));
        assert!(
            futures::poll!(joined.as_mut()).is_pending(),
            "the second read joins the download in flight"
        );
        cancelled.cancel();
        joined.await
    });

    assert!(
        matches!(first, Err(ArtifactLoadError::Cancelled { .. })),
        "{first:?}"
    );
    assert_eq!(second.expect("BUG: the second read decodes").revision(), 3);
    assert_eq!(served.requests.count(), 2);
}

/// A read waiting on another read's download holds no blocking thread: with
/// a single one, the download still stores its document and both decode.
#[kithara::test(native, flash(false))]
fn a_read_waiting_on_a_download_holds_no_blocking_thread() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("BUG: a runtime with one blocking thread builds");
    runtime.block_on(async {
        let served = Served::holding_first(document()).await;
        let config = track(audio(), memory_store());
        let request = request(served.url());
        let cancel = CancelToken::never().child();
        let fetch = config.artifact_fetch();

        let both = async {
            tokio::join!(fetch.read::<TrackAnalysis>(&request, &cancel), async {
                served.arrived().await;
                let mut joined = pin!(fetch.read::<TrackAnalysis>(&request, &cancel));
                assert!(
                    futures::poll!(joined.as_mut()).is_pending(),
                    "the second read joins the download in flight"
                );
                served.release.open();
                joined.await
            })
        };
        let (first, second) = timeout(Duration::from_secs(10), both)
            .await
            .expect("the download stored its document while a read waited on it");

        assert!(first.is_ok() && second.is_ok(), "{first:?} {second:?}");
    });
}

/// The document type of a read that is cancelled while its document is
/// decoded, after the download and before the store keeps it.
struct CancelsItsRead;

static DECODE_CANCELS: OnceLock<CancelToken> = OnceLock::new();

impl ArtifactDocument for CancelsItsRead {
    const KIND: &'static str = "self-cancelling document";

    fn decode(_bytes: Vec<u8>) -> Result<Self, String> {
        DECODE_CANCELS
            .get()
            .expect("BUG: the test sets the token first")
            .cancel();
        Ok(Self)
    }
}

/// A read cancelled once its document is downloaded stores nothing.
#[kithara::test(native, tokio)]
async fn a_read_cancelled_before_its_document_is_stored_stores_nothing(temp_dir: TestTempDir) {
    let served = Served::new(document()).await;
    let config = track(audio(), disk_store(&temp_dir));
    let cancel = CancelToken::never().child();
    assert!(DECODE_CANCELS.set(cancel.clone()).is_ok());

    let error = config
        .artifact_fetch()
        .read::<CancelsItsRead>(&request(served.url()), &cancel)
        .await
        .err()
        .expect("BUG: a cancelled read reads nothing");

    assert!(
        matches!(error, ArtifactLoadError::Cancelled { .. }),
        "{error}"
    );
    let key = config
        .asset_key(&AssetResource::Url(served.url()))
        .expect("BUG: the request URL keys inside the track's scope");
    assert!(
        !matches!(
            config.store().resource_state(&key),
            Ok(AssetResourceState::Committed { .. })
        ),
        "a cancelled read stored its document"
    );
}

fn foreign_document() -> Vec<u8> {
    let mut bytes = document();
    bytes[..4].copy_from_slice(&0x4b41_00ff_u32.to_le_bytes());
    bytes
}

/// A stored document this build does not read is removed by the read that
/// finds it, so the next read downloads the document again.
#[kithara::test(native, tokio)]
#[case::foreign_version(foreign_document())]
#[case::past_the_cap(vec![0_u8; MAX_ARTIFACT_BYTES + 1])]
async fn a_stored_document_that_does_not_read_is_downloaded_again(
    #[case] stored: Vec<u8>,
    temp_dir: TestTempDir,
) {
    let served = Served::new(document()).await;
    let config = track(audio(), disk_store(&temp_dir));
    store_document(&config, &served.url(), &stored);
    let fetch = config.artifact_fetch();
    let cancel = CancelToken::never().child();

    let refused = fetch
        .read::<TrackAnalysis>(&request(served.url()), &cancel)
        .await
        .expect_err("BUG: the stored document does not read");
    let read = fetch
        .read::<TrackAnalysis>(&request(served.url()), &cancel)
        .await
        .expect("BUG: the downloaded document reads");

    assert!(
        matches!(
            refused,
            ArtifactLoadError::Decode { .. } | ArtifactLoadError::TooLarge { .. }
        ),
        "{refused}"
    );
    assert_eq!(read.revision(), 3);
    assert_eq!(served.requests.count(), 1);
}

/// What a read leaves in the store when its document never reads.
async fn nothing_stored(served: &Served, temp_dir: &TestTempDir) -> ArtifactLoadError {
    let config = track(audio(), disk_store(temp_dir));
    let error = config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request(served.url()), &CancelToken::never().child())
        .await
        .expect_err("BUG: the document must not read");
    let key = config
        .asset_key(&AssetResource::Url(served.url()))
        .expect("BUG: the request URL keys inside the track's scope");
    assert!(
        !matches!(
            config.store().resource_state(&key),
            Ok(AssetResourceState::Committed { .. })
        ),
        "a document that never read was stored"
    );
    error
}

#[kithara::test(native, tokio)]
async fn an_error_answer_is_not_stored(temp_dir: TestTempDir) {
    let served = Served::serve(Vec::new(), StatusCode::NOT_FOUND, false).await;

    let error = nothing_stored(&served, &temp_dir).await;

    assert!(matches!(error, ArtifactLoadError::Fetch { .. }), "{error}");
}

#[kithara::test(native, tokio)]
async fn refused_bytes_are_not_stored(temp_dir: TestTempDir) {
    let served = Served::new(b"not a snapshot".to_vec()).await;

    let error = nothing_stored(&served, &temp_dir).await;

    assert!(
        matches!(error, ArtifactLoadError::Decode { kind, .. } if kind == "analysis"),
        "{error}"
    );
}

#[kithara::test(native, tokio)]
async fn a_document_past_the_cap_is_not_stored(temp_dir: TestTempDir) {
    let served = Served::new(vec![0_u8; MAX_ARTIFACT_BYTES + 1]).await;

    let error = nothing_stored(&served, &temp_dir).await;

    assert!(
        matches!(error, ArtifactLoadError::TooLarge { limit, .. } if limit == MAX_ARTIFACT_BYTES),
        "a declared length past the cap is refused before its body: {error}"
    );
}

/// The request URL names the document: another address is another document,
/// downloaded even while the track's old one is stored.
#[kithara::test(native, tokio)]
async fn a_new_address_is_downloaded_beside_a_stored_one(temp_dir: TestTempDir) {
    let served = Served::new(document()).await;
    let config = track(audio(), disk_store(&temp_dir));
    let fetch = config.artifact_fetch();
    let cancel = CancelToken::never().child();
    fetch
        .read::<TrackAnalysis>(&request(served.url()), &cancel)
        .await
        .expect("BUG: the old document reads");

    fetch
        .read::<TrackAnalysis>(&request(served.server.url("/other")), &cancel)
        .await
        .expect("BUG: the new document reads");

    assert_eq!(served.requests.count(), 2);
}

#[kithara::test(tokio)]
async fn a_missing_path_is_an_error() {
    let config = track(audio(), memory_store());

    let error = config
        .artifact_fetch()
        .load::<Cover>(&ResourceSrc::Path(PathBuf::from(
            "/kithara/no/such/cover.jpg",
        )))
        .await
        .expect_err("BUG: nothing to read");

    assert!(matches!(error, ArtifactLoadError::Fetch { .. }), "{error}");
}

#[kithara::test(tokio)]
async fn a_file_past_the_cap_is_refused_unread() {
    let path = file("huge.bin", &vec![0_u8; MAX_ARTIFACT_BYTES + 1]);
    let config = track(audio(), memory_store());

    let error = config
        .artifact_fetch()
        .load::<Cover>(&ResourceSrc::Path(path))
        .await
        .expect_err("BUG: past the cap");

    assert!(
        matches!(error, ArtifactLoadError::TooLarge { limit, .. } if limit == MAX_ARTIFACT_BYTES),
        "{error}"
    );
}

/// A body that declares no length is held to the cap while it streams.
#[kithara::test(tokio)]
async fn a_streamed_body_past_the_cap_is_refused() {
    const CHUNK: usize = 64 * 1024;
    let chunks = MAX_ARTIFACT_BYTES / CHUNK + 1;
    let server = TestHttpServer::new(Router::new().route(
        "/artifact",
        get(move || async move {
            Body::from_stream(stream::iter(
                (0..chunks).map(|_| Ok::<_, Infallible>(vec![0_u8; CHUNK])),
            ))
        }),
    ))
    .await;
    let config = track(audio(), memory_store());

    let error = config
        .artifact_fetch()
        .load::<Cover>(&ResourceSrc::Url(server.url("/artifact")))
        .await
        .expect_err("BUG: past the cap");

    assert!(
        matches!(error, ArtifactLoadError::TooLarge { limit, .. } if limit == MAX_ARTIFACT_BYTES),
        "{error}"
    );
}

#[kithara::test(tokio)]
async fn a_scheme_no_artifact_is_read_over_is_refused() {
    let config = track(audio(), memory_store());
    let url = Url::parse("ftp://example.com/analysis.bin").expect("BUG: valid URL");

    let error = config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request(url), &CancelToken::never().child())
        .await
        .expect_err("BUG: not a read scheme");

    assert!(matches!(error, ArtifactLoadError::Scheme { .. }), "{error}");
}

/// A read whose token is over before it starts reads nothing.
#[kithara::test(tokio)]
async fn a_cancelled_read_reports_itself_cancelled() {
    let served = Served::new(document()).await;
    let config = track(audio(), memory_store());
    let cancel = CancelToken::never().child();
    cancel.cancel();

    let error = config
        .artifact_fetch()
        .read::<TrackAnalysis>(&request(served.url()), &cancel)
        .await
        .expect_err("BUG: cancelled");

    assert!(
        matches!(error, ArtifactLoadError::Cancelled { .. }),
        "{error}"
    );
    assert_eq!(served.requests.count(), 0);
}
