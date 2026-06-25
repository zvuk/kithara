#![cfg(not(target_arch = "wasm32"))]

//! Cache-first size estimation: reopening a track over the same asset
//! store must not network-probe resources whose committed `final_len`
//! is already known. Probes = `HEAD` or `GET` with `Range: bytes=0-0`.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::Response,
};
use kithara_assets::{
    AssetScopeDelegate, AssetStore, AssetStoreBuilder, ResourceKey, StoreOptions,
};
use kithara_hls::{Hls, HlsAssetScopeDelegate, HlsConfig};
use kithara_platform::{
    time::{Duration, Instant, sleep},
    tokio::{net::TcpListener as TokioTcpListener, task::spawn as tokio_spawn},
};
use kithara_stream::Stream;
use kithara_test_utils::kithara;
use url::Url;

const INIT_LEN: usize = 100;
const SEG_LENS: [usize; 4] = [1000, 1100, 1200, 1300];
const TOTAL_LEN: u64 = 4700;
const MEDIA_PATHS: [&str; 5] = ["init.mp4", "seg0.m4s", "seg1.m4s", "seg2.m4s", "seg3.m4s"];
const COMMIT_DEADLINE: Duration = Duration::from_secs(15);
const COMMIT_POLL: Duration = Duration::from_millis(20);

// File-like media keeps the exact-size estimator. Segment-aware fMP4 skips
// network probes at startup, so cache-first estimator behavior is pinned here
// with the test-server's explicit `wav` CODECS marker.
const MASTER_PLAYLIST: &str = "#EXTM3U\n\
    #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"wav\"\n\
    media.m3u8\n";

const MEDIA_PLAYLIST: &str = "#EXTM3U\n\
    #EXT-X-VERSION:6\n\
    #EXT-X-TARGETDURATION:4\n\
    #EXT-X-PLAYLIST-TYPE:VOD\n\
    #EXT-X-MAP:URI=\"init.mp4\"\n\
    #EXTINF:4.0,\n\
    seg0.m4s\n\
    #EXTINF:4.0,\n\
    seg1.m4s\n\
    #EXTINF:4.0,\n\
    seg2.m4s\n\
    #EXTINF:4.0,\n\
    seg3.m4s\n\
    #EXT-X-ENDLIST\n";

type ProbeCounts = Arc<Mutex<HashMap<String, usize>>>;

struct TestServer {
    probes: ProbeCounts,
    master_url: Url,
}

impl TestServer {
    fn media_probe_count(&self, path: &str) -> usize {
        self.probes
            .lock()
            .expect("probes lock")
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    fn probe_snapshot(&self) -> HashMap<String, usize> {
        self.probes.lock().expect("probes lock").clone()
    }

    fn reset_probes(&self) {
        self.probes.lock().expect("probes lock").clear();
    }
}

fn test_files() -> HashMap<String, Vec<u8>> {
    let mut files = HashMap::new();
    files.insert(
        "master.m3u8".to_owned(),
        MASTER_PLAYLIST.as_bytes().to_vec(),
    );
    files.insert("media.m3u8".to_owned(), MEDIA_PLAYLIST.as_bytes().to_vec());
    files.insert("init.mp4".to_owned(), vec![b'i'; INIT_LEN]);
    for (idx, len) in SEG_LENS.iter().enumerate() {
        files.insert(
            format!("seg{idx}.m4s"),
            vec![b'a' + u8::try_from(idx).expect("small idx"); *len],
        );
    }
    files
}

fn parse_range(spec: &str, len: usize) -> Option<(usize, usize)> {
    let rest = spec.strip_prefix("bytes=")?;
    let (start, end) = rest.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end: usize = if end.is_empty() {
        len.checked_sub(1)?
    } else {
        end.parse().ok()?
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end.min(len - 1)))
}

fn handle(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    files: &HashMap<String, Vec<u8>>,
    probes: &ProbeCounts,
) -> Response {
    let path = uri.path().trim_start_matches('/').to_owned();
    let Some(data) = files.get(&path) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("response");
    };
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let is_probe = *method == Method::HEAD || range.as_deref() == Some("bytes=0-0");
    if is_probe {
        *probes.lock().expect("probes lock").entry(path).or_insert(0) += 1;
    }
    if *method == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, data.len())
            .body(Body::empty())
            .expect("response");
    }
    match range.as_deref().and_then(|r| parse_range(r, data.len())) {
        Some((start, end)) => Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", data.len()),
            )
            .header(header::CONTENT_LENGTH, end - start + 1)
            .body(Body::from(data[start..=end].to_vec()))
            .expect("response"),
        None => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, data.len())
            .body(Body::from(data.clone()))
            .expect("response"),
    }
}

async fn spawn_server() -> TestServer {
    let files = Arc::new(test_files());
    let probes: ProbeCounts = Arc::new(Mutex::new(HashMap::new()));
    let probes_handler = Arc::clone(&probes);
    let app = Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap| {
        let files = Arc::clone(&files);
        let probes = Arc::clone(&probes_handler);
        async move { handle(&method, &uri, &headers, &files, &probes) }
    });
    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    let master_url = Url::parse(&format!("http://{addr}/master.m3u8")).expect("master url");
    TestServer { probes, master_url }
}

/// Verification-only store over the same `cache_dir`: read-only
/// `final_len` queries against committed on-disk resources.
fn verify_store(cache_dir: &Path) -> AssetStore {
    AssetStoreBuilder::new().root_dir(cache_dir).build()
}

fn media_keys(store: &AssetStore, master_url: &Url, paths: &[&str]) -> Vec<(ResourceKey, u64)> {
    let delegate: Arc<dyn AssetScopeDelegate> = Arc::new(HlsAssetScopeDelegate);
    let asset_root = delegate.asset_root_for_url(master_url, None);
    let scope = store.scope_with_delegate(asset_root, delegate);
    paths
        .iter()
        .map(|path| {
            let url = master_url.join(path).expect("resource url");
            let len = match *path {
                "init.mp4" => INIT_LEN,
                other => {
                    let idx: usize = other[3..4].parse().expect("segment idx");
                    SEG_LENS[idx]
                }
            };
            (scope.key_from_url(&url), len as u64)
        })
        .collect()
}

async fn wait_committed(store: &AssetStore, keys: &[(ResourceKey, u64)]) {
    let deadline = Instant::now() + COMMIT_DEADLINE;
    loop {
        if keys
            .iter()
            .all(|(key, len)| store.final_len(key) == Some(*len))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for resources to commit"
        );
        sleep(COMMIT_POLL).await;
    }
}

fn config(master_url: &Url, cache_dir: &Path) -> HlsConfig {
    HlsConfig::for_url(master_url.clone())
        .store(StoreOptions::new(cache_dir))
        .build()
}

#[kithara::test(tokio, timeout(Duration::from_secs(60)))]
async fn reopen_fully_cached_track_probes_nothing() {
    let server = spawn_server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_dir = tmp.path();

    let stream1 = Stream::<Hls>::new(config(&server.master_url, cache_dir))
        .await
        .expect("first open");
    assert_eq!(stream1.len(), Some(TOTAL_LEN), "first open size map total");

    let store = verify_store(cache_dir);
    let keys = media_keys(&store, &server.master_url, &MEDIA_PATHS);
    wait_committed(&store, &keys).await;
    drop(stream1);

    server.reset_probes();
    let stream2 = Stream::<Hls>::new(config(&server.master_url, cache_dir))
        .await
        .expect("second open");

    let probes = server.probe_snapshot();
    let total_probes: usize = probes.values().sum();
    assert_eq!(
        total_probes, 0,
        "fully cached reopen must not probe the network, got {probes:?}"
    );
    assert_eq!(stream2.len(), Some(TOTAL_LEN), "second open size map total");
}

#[kithara::test(tokio, timeout(Duration::from_secs(60)))]
async fn reopen_partially_cached_track_probes_only_missing() {
    // Cap = 1500 variant bytes: seg0 (offset 0) and seg1 (offset 1100)
    // download, seg2 (2200) and seg3 (3400) stay out of reach.
    const PARTIAL_LOOK_AHEAD: u64 = 1500;
    const CACHED: [&str; 3] = ["init.mp4", "seg0.m4s", "seg1.m4s"];
    const MISSING: [&str; 2] = ["seg2.m4s", "seg3.m4s"];

    let server = spawn_server().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_dir = tmp.path();

    let partial_config = HlsConfig::for_url(server.master_url.clone())
        .store(StoreOptions::new(cache_dir))
        .look_ahead_bytes(PARTIAL_LOOK_AHEAD)
        .build();
    let stream1 = Stream::<Hls>::new(partial_config)
        .await
        .expect("first open");
    assert_eq!(stream1.len(), Some(TOTAL_LEN), "first open size map total");

    let store = verify_store(cache_dir);
    let cached_keys = media_keys(&store, &server.master_url, &CACHED);
    wait_committed(&store, &cached_keys).await;
    drop(stream1);

    server.reset_probes();
    let stream2 = Stream::<Hls>::new(config(&server.master_url, cache_dir))
        .await
        .expect("second open");

    let probes = server.probe_snapshot();
    for path in CACHED {
        assert_eq!(
            server.media_probe_count(path),
            0,
            "cached resource {path} must not be probed, got {probes:?}"
        );
    }
    for path in MISSING {
        assert_eq!(
            server.media_probe_count(path),
            1,
            "missing resource {path} must be probed exactly once, got {probes:?}"
        );
    }
    assert_eq!(stream2.len(), Some(TOTAL_LEN), "second open size map total");
}
