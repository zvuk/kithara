#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, routing::get};
use bytes::Bytes;
use kithara_assets::{
    AcquisitionResult, AssetResource, AssetResourceState, AssetScope, AssetSource, AssetStore,
    AssetStoreBuilder, StorageBackend, WriteSide,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Notify},
    tokio::{
        net::TcpListener,
        task::{spawn, yield_now},
    },
};
use kithara_stream::dl::{Downloader, DownloaderConfig, Peer};
use kithara_test_utils::kithara;
use tempfile::tempdir;

use super::*;

const VALID_MASTER: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000\naudio.m3u8\n";

struct MockPeer;

impl kithara_abr::Abr for MockPeer {}
impl Peer for MockPeer {}

async fn playlist_server(body: Bytes) -> (Url, Arc<AtomicUsize>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/master.m3u8",
        get(move || {
            let body = body.clone();
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                body
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let url = Url::parse(&format!("http://{addr}/master.m3u8")).expect("url");
    (url, requests)
}

async fn gated_playlist_server(body: Bytes) -> (Url, Arc<AtomicUsize>, Arc<Notify>, Arc<Notify>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let first_seen = Arc::new(Notify::default());
    let release = Arc::new(Notify::default());
    let handler_requests = Arc::clone(&requests);
    let handler_first_seen = Arc::clone(&first_seen);
    let handler_release = Arc::clone(&release);
    let app = Router::new().route(
        "/master.m3u8",
        get(move || {
            let body = body.clone();
            let requests = Arc::clone(&handler_requests);
            let first_seen = Arc::clone(&handler_first_seen);
            let release = Arc::clone(&handler_release);
            async move {
                if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                    first_seen.notify_one();
                }
                release.notified().await;
                body
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let url = Url::parse(&format!("http://{addr}/master.m3u8")).expect("url");
    (url, requests, first_seen, release)
}

fn test_cache(scope: AssetScope) -> PlaylistCache {
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let handle = downloader.register(Arc::new(MockPeer));
    PlaylistCache::new(scope, handle, kithara_bufpool::BytePool::default())
}

fn test_scope(store: &AssetStore, url: &Url, discriminator: &str) -> AssetScope {
    store
        .scope::<kithara_hls::Hls>(&AssetSource::Remote {
            url: url.clone(),
            discriminator: Some(discriminator.to_owned()),
        })
        .expect("test asset scope")
}

fn commit(scope: &AssetScope, key: &ResourceKey, bytes: &[u8]) {
    let AcquisitionResult::Pending(writer) =
        scope.store().acquire_resource(key, None).expect("acquire")
    else {
        panic!("resource must be missing before test commit");
    };
    writer.write_at(0, bytes).expect("write");
    writer.commit(Some(bytes.len() as u64)).expect("commit");
}

#[kithara::test(tokio)]
async fn corrupt_persisted_playlist_is_invalidated_and_refetched_once() {
    let (url, requests) = playlist_server(Bytes::from_static(VALID_MASTER)).await;
    let dir = tempdir().expect("tempdir");
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "corrupt-playlist");
    let key = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("playlist key");
    let corrupt = b"\x1b\xbf\x01\x00brotli bytes";
    commit(&scope, &key, corrupt);
    assert!(scope.store().contains_range(&key, 0..corrupt.len() as u64));
    store.checkpoint().expect("persist poisoned index");
    drop(scope);
    drop(store);

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "corrupt-playlist");
    assert!(scope.store().contains_range(&key, 0..corrupt.len() as u64));

    let parsed = test_cache(scope.clone())
        .master_playlist(&key, &url)
        .await
        .expect("corrupt cache must self-heal from the network");

    assert_eq!(parsed.variants.len(), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    store.checkpoint().expect("persist repaired index");
    drop(scope);
    drop(store);

    let reopened = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let parsed_again = test_cache(test_scope(&reopened, &url, "corrupt-playlist"))
        .master_playlist(&key, &url)
        .await
        .expect("replacement bytes must be cached and parseable");
    assert_eq!(parsed_again.variants.len(), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio)]
async fn empty_persisted_playlist_is_invalidated_and_refetched_once() {
    let (url, requests) = playlist_server(Bytes::from_static(VALID_MASTER)).await;
    let dir = tempdir().expect("tempdir");
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "empty-playlist");
    let key = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("playlist key");
    commit(&scope, &key, b"");
    store.checkpoint().expect("persist empty index entry");
    drop(scope);
    drop(store);

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "empty-playlist");
    let parsed = test_cache(scope.clone())
        .master_playlist(&key, &url)
        .await
        .expect("empty cache must self-heal from the network");

    assert_eq!(parsed.variants.len(), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    store.checkpoint().expect("persist repaired index");
    drop(scope);
    drop(store);

    let reopened = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    test_cache(test_scope(&reopened, &url, "empty-playlist"))
        .master_playlist(&key, &url)
        .await
        .expect("replacement bytes must survive reopen");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio)]
async fn concurrent_caches_share_one_playlist_repair() {
    let (url, requests, first_seen, release) =
        gated_playlist_server(Bytes::from_static(VALID_MASTER)).await;
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "concurrent-playlist-repair");
    let key = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("playlist key");
    commit(&scope, &key, b"\x1b\xbf\x01\x00cached brotli bytes");

    let first_cache = test_cache(scope.clone());
    let second_cache = test_cache(scope);
    let first_key = key.clone();
    let first_url = url.clone();
    let first = spawn(async move { first_cache.master_playlist(&first_key, &first_url).await });
    first_seen.notified().await;

    let second_polled = Arc::new(Notify::default());
    let task_second_polled = Arc::clone(&second_polled);
    let second = spawn(async move {
        task_second_polled.notify_one();
        second_cache.master_playlist(&key, &url).await
    });
    second_polled.notified().await;
    yield_now().await;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    release.notify_one();

    let first = first.await.expect("first task").expect("first playlist");
    let second = second.await.expect("second task").expect("second playlist");
    assert_eq!(first.variants.len(), 1);
    assert_eq!(second.variants.len(), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio)]
async fn failed_refetch_does_not_resurrect_poisoned_index() {
    let invalid = Bytes::from_static(b"\x1b\xbf\x01\x00network brotli bytes");
    let (url, requests) = playlist_server(invalid).await;
    let dir = tempdir().expect("tempdir");
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "failed-playlist-refetch");
    let key = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("playlist key");
    let corrupt = b"\x1b\xbf\x01\x00cached brotli bytes";
    commit(&scope, &key, corrupt);
    store.checkpoint().expect("persist poisoned index");
    drop(scope);
    drop(store);

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "failed-playlist-refetch");
    let error = test_cache(scope.clone())
        .master_playlist(&key, &url)
        .await
        .expect_err("invalid replacement must fail parsing");
    assert!(matches!(error, HlsError::PlaylistParse(_)));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(matches!(
        scope.store().resource_state(&key).expect("resource state"),
        AssetResourceState::Missing
    ));
    assert!(!scope.store().contains_range(&key, 0..corrupt.len() as u64));
    store.checkpoint().expect("persist invalidation");
    drop(scope);
    drop(store);

    let reopened = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .cancel(CancelToken::never())
        .build();
    assert!(matches!(
        reopened.resource_state(&key).expect("reopened state"),
        AssetResourceState::Missing
    ));
    assert!(!reopened.contains_range(&key, 0..corrupt.len() as u64));
}

#[kithara::test(tokio)]
async fn invalid_network_playlist_is_not_cached() {
    let invalid = Bytes::from_static(b"\x1b\xbf\x01\x00brotli bytes");
    let (url, requests) = playlist_server(invalid).await;
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .cancel(CancelToken::never())
        .build();
    let scope = test_scope(&store, &url, "invalid-network-playlist");
    let key = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("playlist key");

    let err = test_cache(scope.clone())
        .master_playlist(&key, &url)
        .await
        .expect_err("invalid network playlist must fail parsing");

    assert!(matches!(err, HlsError::PlaylistParse(_)));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(matches!(
        scope.store().resource_state(&key).expect("resource state"),
        AssetResourceState::Missing
    ));
}
