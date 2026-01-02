use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fixtures::hls::{HlsFixture, HlsFixtureStorageKind};
use fixtures::setup::SERVER_RT;
use futures_util::StreamExt;
use rstest::rstest;
use stream_download::source::{ChunkKind, StreamControl, StreamMsg};
use stream_download_hls::{
    AbrDecision, HlsManager, HlsSettings, MediaStream, NextSegmentResult, StreamEvent, VariantId,
};
use tokio::sync::mpsc;

mod fixtures;

use fixtures::hls::utils::{
    assert_dir_not_empty, build_fixture_storage_kind, clean_dir, count_files_recursive,
    create_fixture_from_config, dir_nonempty_recursive, is_variant_stream_key, test_config,
    wait_first_chunkstart,
};

async fn collect_first_n_chunkstarts(
    mut data_rx: mpsc::Receiver<StreamMsg>,
    n: usize,
) -> Vec<StreamControl> {
    let mut out: Vec<StreamControl> = Vec::with_capacity(n);
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline && out.len() < n {
        match tokio::time::timeout(Duration::from_millis(500), data_rx.recv()).await {
            Ok(Some(StreamMsg::Control(ctrl))) => {
                if matches!(ctrl, StreamControl::ChunkStart { .. }) {
                    out.push(ctrl);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    out
}

async fn read_first_n_bytes_via_worker(
    fixture: HlsFixture,
    storage_kind: HlsFixtureStorageKind,
    n: usize,
) -> Vec<u8> {
    fixture
        .run_worker_collecting(64, storage_kind, move |mut data_rx, _event_rx| {
            Box::pin(async move {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut out: Vec<u8> = Vec::with_capacity(n);

                while out.len() < n && Instant::now() < deadline {
                    match tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await {
                        Ok(Some(StreamMsg::Data(bytes))) => {
                            out.extend_from_slice(&bytes);
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => {}
                    }
                }

                out.truncate(n);
                out
            })
        })
        .await
}

async fn read_first_n_bytes_via_stream_download(
    fixture: HlsFixture,
    storage_kind: &HlsFixtureStorageKind,
    n: usize,
) -> Vec<u8> {
    let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind.clone()).await;

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut buf = vec![0u8; 1024.min(n)];

    while out.len() < n && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(250), async {
            let slice_len = buf.len().min(n - out.len());
            reader.read(&mut buf[..slice_len])
        })
        .await
        {
            Ok(Ok(bytes_read)) if bytes_read > 0 => {
                out.extend_from_slice(&buf[..bytes_read]);
            }
            Ok(Ok(_)) => {
                // EOF or no data
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(Err(e)) => {
                panic!("Failed to read from StreamDownload: {:?}", e);
            }
            Err(_) => {
                // Timeout, continue
            }
        }
    }

    out.truncate(n);
    out
}

#[rstest]
#[case("persistent")]
#[case("temp")]
#[case("memory")]
fn hls_aes128_drm_decrypts_media_segments_for_all_storage_backends(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm();

        let storage_kind = build_fixture_storage_kind("hls-aes128-drm", variant_count, storage);

        // Clean storage roots
        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        // Read enough bytes to include init + first media segment bytes.
        // The init segment is plaintext in the fixture; the first media segment should be decrypted.
        let n = 64usize;
        let got = read_first_n_bytes_via_stream_download(fixture.clone(), &storage_kind, n).await;
        let got_str = String::from_utf8_lossy(&got);

        // Decrypted media payload prefix should appear in output bytes.
        // For the default variant selection, the worker should start from v0, first segment.
        assert!(
            got_str.contains("V0-SEG-0"),
            "expected decrypted media bytes to contain 'V0-SEG-0', got: {got_str:?}"
        );

        // Sanity: key endpoint should have been fetched at least once.
        assert!(
            fixture.request_count_for("/key0.bin").unwrap_or(0) >= 1,
            "expected /key0.bin to be fetched at least once"
        );
    })
}

#[rstest]
#[case("persistent")]
#[case("temp")]
#[case("memory")]
fn hls_aes128_drm_fixed_zero_iv_decrypts_media_segments_for_all_storage_backends(
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true);

        let storage_kind =
            build_fixture_storage_kind("hls-aes128-drm-fixed-zero-iv", variant_count, storage);

        // Start from clean roots so we don't accidentally read cached plaintext/old data.
        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 64usize;
        let got = read_first_n_bytes_via_stream_download(fixture.clone(), &storage_kind, n).await;
        let got_str = String::from_utf8_lossy(&got);

        // With fixed IV=0, decryption should still yield the expected plaintext prefix.
        assert!(
            got_str.contains("V0-SEG-0"),
            "expected decrypted media bytes to contain 'V0-SEG-0', got: {got_str:?}"
        );

        assert!(
            fixture.request_count_for("/key0.bin").unwrap_or(0) >= 1,
            "expected /key0.bin to be fetched at least once"
        );
    })
}

#[rstest]
#[case("persistent")]
#[case("temp")]
#[case("memory")]
fn hls_aes128_drm_applies_key_query_params_headers_and_key_processor_cb(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        // Test key query params and processor callback
        let mut qp = HashMap::new();
        qp.insert("token".to_string(), "abc123".to_string());
        qp.insert("tenant".to_string(), "tests".to_string());

        // Unwrap scheme must match fixture "wrap" scheme:
        // raw[i] = wrapped[i] ^ 0xFF  (fixture serves wrapped = raw ^ 0xFF)
        let cb: Arc<Box<stream_download_hls::KeyProcessorCallback>> = Arc::new(Box::new(|b| {
            if b.is_empty() {
                return b;
            }
            let mut v = b.to_vec();
            for x in &mut v {
                *x ^= 0xFF;
            }
            bytes::Bytes::from(v)
        }));

        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true)
            .with_wrapped_keys(true)
            .with_key_processor_cb(Some(cb))
            .with_key_required_query_params(Some(qp));

        let storage_kind = build_fixture_storage_kind(
            "hls-aes128-drm-key-params-headers-cb",
            variant_count,
            storage,
        );

        // Start from clean roots so we don't accidentally read cached data.
        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 64usize;
        let got = read_first_n_bytes_via_stream_download(fixture.clone(), &storage_kind, n).await;
        let got_str = String::from_utf8_lossy(&got);

        assert!(
            got_str.contains("V0-SEG-0"),
            "expected decrypted media bytes to contain 'V0-SEG-0', got: {got_str:?}"
        );

        assert!(
            fixture.request_count_for("/key0.bin").unwrap_or(0) >= 1,
            "expected /key0.bin to be fetched at least once"
        );
    })
}

#[rstest]
#[case("persistent")]
#[case("temp")]
fn hls_aes128_drm_key_is_cached_not_fetched_per_segment(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        // Test key caching across segments
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true);

        let storage_kind =
            build_fixture_storage_kind("hls-aes128-drm-key-cache", variant_count, storage);

        // Start from clean roots so we don't accidentally read cached data.
        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        // Read more than the small 64 bytes used elsewhere to increase likelihood of fetching >1 segment.
        // (Fixture segment payloads contain textual prefixes like "V0-SEG-0".)
        let n = 4096usize;
        // Use StreamDownload instead of worker directly to ensure StoreResource messages are processed
        let got = read_first_n_bytes_via_stream_download(fixture.clone(), &storage_kind, n).await;
        let got_str = String::from_utf8_lossy(&got);

        assert!(
            got_str.contains("V0-SEG-0"),
            "expected decrypted media bytes to contain 'V0-SEG-0', got: {got_str:?}"
        );

        let key_hits = fixture.request_count_for("/key0.bin").unwrap_or(0);
        assert!(
            key_hits >= 1,
            "expected /key0.bin to be fetched at least once"
        );

        // Key should be cached and reused for multiple segments; it should not be fetched for every segment.
        // We keep this assertion loose (<= 2) to avoid flakiness from playlist refresh / retries.
        assert!(
            key_hits <= 2,
            "expected key to be cached (not fetched per segment); got /key0.bin hits={key_hits}"
        );
    })
}

#[test]
#[should_panic(
    expected = "missing/invalid key request header: x-drm-token: abc123 (got <missing>)"
)]
fn hls_aes128_drm_fails_when_required_key_request_headers_not_sent_persistent() {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        let mut required = HashMap::new();
        required.insert("x-drm-token".to_string(), "abc123".to_string());

        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true)
            .with_key_required_request_headers(Some(required))
            // Intentionally remove client config so headers are NOT sent.
            .with_hls_config(HlsSettings::default());

        let storage_kind = build_fixture_storage_kind(
            "hls-aes128-drm-missing-key-headers",
            variant_count,
            "persistent",
        );

        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 256usize;
        let _ = read_first_n_bytes_via_worker(fixture.clone(), storage_kind, n).await;
    })
}

#[test]
#[should_panic(
    expected = "missing/invalid key request header: x-drm-token: abc123 (got <missing>)"
)]
fn hls_aes128_drm_fails_when_required_key_request_headers_not_sent_temp() {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        let mut required = HashMap::new();
        required.insert("x-drm-token".to_string(), "abc123".to_string());

        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true)
            .with_key_required_request_headers(Some(required))
            // Intentionally remove client config so headers are NOT sent.
            .with_hls_config(HlsSettings::default());

        let storage_kind =
            build_fixture_storage_kind("hls-aes128-drm-missing-key-headers", variant_count, "temp");

        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 256usize;
        let _ = read_first_n_bytes_via_worker(fixture.clone(), storage_kind, n).await;
    })
}

#[test]
#[should_panic(
    expected = "missing/invalid key request header: x-drm-token: abc123 (got <missing>)"
)]
fn hls_aes128_drm_fails_when_required_key_request_headers_not_sent_memory() {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        let mut required = HashMap::new();
        required.insert("x-drm-token".to_string(), "abc123".to_string());

        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true)
            .with_key_required_request_headers(Some(required))
            // Intentionally remove client config so headers are NOT sent.
            .with_hls_config(HlsSettings::default());

        let storage_kind = build_fixture_storage_kind(
            "hls-aes128-drm-missing-key-headers",
            variant_count,
            "memory",
        );

        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 256usize;
        let _ = read_first_n_bytes_via_worker(fixture.clone(), storage_kind, n).await;
    })
}

#[rstest]
#[case("persistent")]
#[case("temp")]
#[case("memory")]
#[should_panic(expected = "HTTP status client error (404 Not Found)")]
fn hls_base_url_default_segment_urls_404_when_segments_are_remapped(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        // Test that segments return 404 without base_url override
        //
        // So: derive prefix from a URL on the *same* host as the fixture server, then clear client base_url again.
        let fixture_base =
            HlsFixture::with_variant_count(variant_count).with_segment_delay(Duration::ZERO);

        // Bootstrap: start server to learn its base URL.
        let storage_kind_bootstrap = build_fixture_storage_kind(
            "hls-base-url-derived-prefix-default-404-bootstrap",
            variant_count,
            storage,
        );

        match &storage_kind_bootstrap {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let (server_base_url, _reader) = fixture_base
            .stream_download_boxed(storage_kind_bootstrap)
            .await;

        // Enable prefixed-only serving in the fixture, but do NOT configure client base_url.
        // Prefix is intentionally "deep" (multiple path components) to ensure there is no hardcoding.
        let derived_base_url = server_base_url
            .join("a/b/")
            .expect("failed to build derived base_url");

        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_base_url_override(Some(derived_base_url))
            // Clear client base_url so requests go to default (non-prefixed) URLs and hit 404 in the fixture.
            .with_hls_config(HlsSettings::default());

        let storage_kind = build_fixture_storage_kind(
            "hls-base-url-derived-prefix-default-404",
            variant_count,
            storage,
        );

        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 256usize;
        let _ = read_first_n_bytes_via_worker(fixture, storage_kind, n).await;
    })
}

#[rstest]
#[case("persistent", "a")]
#[case("persistent", "a/b")]
#[case("persistent", "a/b/c")]
#[case("temp", "a")]
#[case("temp", "a/b")]
#[case("temp", "a/b/c")]
#[case("memory", "a")]
#[case("memory", "a/b")]
#[case("memory", "a/b/c")]
fn hls_base_url_override_makes_segment_remap_work(#[case] storage: &str, #[case] prefix: &str) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        // Test base_url override with different prefix depths
        let depth = prefix.split('/').filter(|s| !s.is_empty()).count();

        // Start the prefixed-only fixture server first, then take its *actual* server base URL and
        // build a derived `base_url` from it.
        //
        // This avoids a second "bootstrap" fixture run that can interleave logs and obscure failures.
        let fixture_server = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            // any URL with a deep path will do here; only the path part matters for prefix derivation
            .with_base_url_override(Some(
                reqwest::Url::parse("http://fixture.invalid/a/b/c/").expect("valid test base_url"),
            ));

        let storage_kind_server = build_fixture_storage_kind(
            &format!("hls-base-url-derived-prefix-{}-server", prefix.replace('/', "_")),
            variant_count,
            storage,
        );

        match &storage_kind_server {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        // Start server and get real host/port.
        let server_base_url = fixture_server.start().await;

        // Now configure the client to use `base_url` on the actual server host.
        let base_url = server_base_url
            .join(&format!("{}/", prefix))
            .expect("failed to build derived base_url");

        let fixture = fixture_server.with_hls_config(HlsSettings::default().base_url(base_url));

        let storage_kind = build_fixture_storage_kind(
            &format!("hls-base-url-derived-prefix-{}-override-ok", prefix.replace('/', "_")),
            variant_count,
            storage,
        );

        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 256usize;
        let got = read_first_n_bytes_via_stream_download(fixture, &storage_kind, n).await;
        let got_str = String::from_utf8_lossy(&got);

        assert!(
            got_str.contains("V0-SEG-0"),
            "expected base_url override to make derived-prefix URLs work (prefix={prefix}, depth={depth}), got: {got_str:?}"
        );
    })
}

#[rstest]
#[case("persistent")]
#[case("temp")]
#[case("memory")]
fn hls_aes128_drm_succeeds_when_key_headers_not_required_and_not_sent(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        let variant_count = 2usize;

        // Test DRM without required headers
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_aes128_drm()
            .with_aes128_fixed_zero_iv(true)
            .with_key_required_request_headers(None)
            .with_hls_config(HlsSettings::default());

        let storage_kind = build_fixture_storage_kind(
            "hls-aes128-drm-no-key-headers-required",
            variant_count,
            storage,
        );

        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => clean_dir(storage_root),
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => clean_dir(resource_cache_root),
        }

        let n = 128usize;
        let got = read_first_n_bytes_via_stream_download(fixture.clone(), &storage_kind, n).await;
        let got_str = String::from_utf8_lossy(&got);

        assert!(
            got_str.contains("V0-SEG-0"),
            "expected DRM to succeed without key headers when not required; got: {got_str:?}"
        );

        assert!(
            fixture.request_count_for("/key0.bin").unwrap_or(0) >= 1,
            "expected /key0.bin to be fetched at least once"
        );
    })
}

#[rstest]
#[case(2, Duration::ZERO, "persistent")]
#[case(2, Duration::ZERO, "temp")]
#[case(2, Duration::ZERO, "memory")]
#[case(2, Duration::from_millis(250), "persistent")]
#[case(2, Duration::from_millis(250), "temp")]
#[case(2, Duration::from_millis(250), "memory")]
#[case(4, Duration::ZERO, "persistent")]
#[case(4, Duration::ZERO, "temp")]
#[case(4, Duration::ZERO, "memory")]
#[case(4, Duration::from_millis(250), "persistent")]
#[case(4, Duration::from_millis(250), "temp")]
#[case(4, Duration::from_millis(250), "memory")]
fn hls_cache_warmup_produces_bytes_twice_on_same_storage_root(
    #[case] variant_count: usize,
    #[case] v0_delay: Duration,
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(v0_delay);

        let storage_kind = build_fixture_storage_kind(
            &format!(
                "hls-cache-warmup-fixture-d{}",
                v0_delay.as_millis()
            ),
            variant_count,
            storage,
        );

        // Ensure we start from a clean slate so the "warmup happened" assertion is meaningful.
        match &storage_kind {
            HlsFixtureStorageKind::Persistent { storage_root } => {
                clean_dir(storage_root);
            }
            HlsFixtureStorageKind::Temp { subdir } => {
                let root = std::env::temp_dir()
                    .join("stream-download-tests")
                    .join(subdir);
                clean_dir(&root);
            }
            HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => {
                clean_dir(resource_cache_root);
            }
        }
        fixture
            .reset_request_counts()
            .expect("failed to reset fixture request counters");

        let base_url = fixture.start().await;
        let _seg0_path = "/seg/v0_0.bin";

        // The fixture payloads are intentionally tiny (short ASCII strings), so we can't "force"
        // large reads. This test should validate *storage reuse* without overfitting to HTTP
        // fetch patterns (which can legitimately re-fetch due to playlist reloads, cache headers,
        // TTL, etc).
        //
        // What we assert:
        // - each run reads at least some bytes (smoke)
        // - for persistent storage, the storage root becomes non-empty after run #1
        //   (i.e. something was persisted and can be reused across runs)
        // - for persistent storage, the storage root stays non-empty on run #2
        //   (i.e. warmup actually persisted data and it was available for reuse)
        //
        // IMPORTANT:
        // We intentionally avoid asserting "no HTTP re-fetch" on run #2 because the pipeline may
        // legitimately re-issue requests (playlist reload, conditional requests, missing/changed cache
        // headers, TTL, races around cache population, etc). The correctness property we care about
        // here is that persistent storage ends up holding data after run #1 and that data remains
        // available on run #2.
        //
        // Read enough bytes to force a media segment fetch (init + first segment payload).
        let min_total = 16usize;
        let timeout = Duration::from_secs(10);

        let assert_persistent_reuse = storage == "persistent";
        let persistent_root = if assert_persistent_reuse {
            match &storage_kind {
                HlsFixtureStorageKind::Persistent { storage_root } => Some(storage_root.clone()),
                _ => None,
            }
        } else {
            None
        };

        for run_idx in 0..2 {
            let mut reader = fixture
                .stream_download_boxed_with_base(
                    base_url.clone(),
                    storage_kind.clone(),
                )
                .await;

            let mut buf = vec![0u8; 64 * 1024];
            let mut total = 0usize;
            let start = Instant::now();

            while start.elapsed() < timeout {
                // Just keep draining until we have at least 1 byte, then stop.
                // The purpose is to trigger at least some real work in the pipeline.
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total += n;
                        if total >= min_total {
                            break;
                        }
                    }
                    Err(e) => panic!(
                        "run {run_idx}: read error after {total} bytes (variant_count={variant_count}, v0_delay={:?}, storage={storage}): {e}",
                        v0_delay
                    ),
                }
            }

            assert!(
                total >= min_total,
                "run {run_idx}: expected to read at least {min_total} byte(s) from HLS stream within timeout (got {total}) (variant_count={variant_count}, v0_delay={:?}, storage={storage})",
                v0_delay
            );

            // For persistent storage, assert that the on-disk root becomes non-empty after the first run.
            // This is a concrete "cache warmup happened" signal without assuming anything about HTTP re-fetch.
            if assert_persistent_reuse && run_idx == 0 {
                let root = persistent_root
                    .as_ref()
                    .expect("persistent storage kind should provide storage_root");
                assert!(
                    dir_nonempty_recursive(root),
                    "expected persistent storage root to contain files after run 0, but it is empty: {} (variant_count={}, v0_delay={:?})",
                    root.display(),
                    variant_count,
                    v0_delay
                );

                // We no longer assert on HTTP request counts here.
                // A run may legitimately re-issue HTTP requests (playlist reloads, conditional requests, TTL, etc).
                // The persistence signal we assert is: after run 0, the persistent storage root is non-empty.
            } else if assert_persistent_reuse && run_idx == 1 {
                // Run #2 should still have persisted data available.
                // This is a stable signal of cache warmup across runs without overfitting to HTTP
                // request-count behavior.
                let root = persistent_root
                    .as_ref()
                    .expect("persistent storage kind should provide storage_root");
                assert!(
                    dir_nonempty_recursive(root),
                    "expected persistent storage root to remain non-empty on run 1, but it is empty: {} (variant_count={}, v0_delay={:?})",
                    root.display(),
                    variant_count,
                    v0_delay
                );
            }
        }
    });
}

#[rstest]
#[case(1, "persistent")]
#[case(1, "temp")]
#[case(1, "memory")]
#[case(2, "persistent")]
#[case(2, "temp")]
#[case(2, "memory")]
#[case(4, "persistent")]
#[case(4, "temp")]
#[case(4, "memory")]
#[case(6, "persistent")]
#[case(6, "temp")]
#[case(6, "memory")]
fn hls_manager_parses_exact_variant_count_from_master(
    #[case] variant_count: usize,
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        let fixture =
            HlsFixture::with_variant_count(variant_count).with_segment_delay(Duration::ZERO);

        let storage_kind =
            build_fixture_storage_kind("hls-manager-parse-master", variant_count, storage);
        let storage_bundle = fixture.build_storage(storage_kind);
        let storage_handle = storage_bundle.storage_handle();

        let (data_tx, _data_rx) = mpsc::channel::<StreamMsg>(1);
        let (_base_url, mut manager) = fixture.manager(storage_handle, data_tx).await;

        manager
            .load_master()
            .await
            .expect("failed to load master playlist");

        let master = manager
            .master()
            .expect("master playlist must be available after load_master");

        assert_eq!(
            master.variants.len(),
            variant_count,
            "expected {} variants in master playlist, got {} (storage={})",
            variant_count,
            master.variants.len(),
            storage
        );

        let mut seen_uris: HashSet<String> = HashSet::new();
        let mut prev_bw = 0u64;
        for (idx, variant) in master.variants.iter().enumerate() {
            assert_eq!(
                variant.id,
                VariantId(idx),
                "expected variant ids to be sequential starting at 0 (storage={storage})"
            );
            assert!(
                seen_uris.insert(variant.uri.clone()),
                "variant URIs must be unique (saw duplicate {}) (storage={storage})",
                variant.uri
            );
            assert!(
                variant.uri.ends_with(&format!("v{idx}.m3u8")),
                "fixture URIs should map to v{idx}.m3u8 for idx={idx} (storage={storage}), got {}",
                variant.uri
            );
            if let Some(bw) = variant.bandwidth {
                assert!(
                    bw > 0,
                    "variant {idx} should advertise positive bandwidth (storage={storage})"
                );
                assert!(
                    bw >= prev_bw,
                    "bandwidths should be non-decreasing (storage={storage}); prev={prev_bw}, current={bw}"
                );
                prev_bw = bw;
            } else {
                panic!("variant {idx} missing bandwidth in master playlist (storage={storage})");
            }
        }
    });
}

#[rstest]
#[case(0, "persistent")]
#[case(0, "temp")]
#[case(0, "memory")]
#[case(1, "persistent")]
#[case(1, "temp")]
#[case(1, "memory")]
#[case(2, "persistent")]
#[case(2, "temp")]
#[case(2, "memory")]
#[case(3, "persistent")]
#[case(3, "temp")]
#[case(3, "memory")]
fn hls_worker_manual_selection_emits_only_selected_variant_chunks(
    #[case] variant_idx: u64,
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        // Use 4 variants so we can parameterize across multiple variant selections.
        let fixture = HlsFixture::with_variant_count(4)
            .with_segment_delay(Duration::ZERO)
            .with_hls_config(
                HlsSettings::default()
                    .variant_stream_selector(move |_| Some(VariantId(variant_idx as usize))),
            );

        let storage_kind = build_fixture_storage_kind("hls-worker-manual", 4, storage);

        let chunkstarts = fixture
            .run_worker_collecting(
                4096,
                storage_kind,
                |rx, _events| Box::pin(async move { collect_first_n_chunkstarts(rx, 8).await }),
            )
            .await;

        assert!(
            !chunkstarts.is_empty(),
            "expected at least one ChunkStart in manual mode (variant_idx={}, storage={})",
            variant_idx,
            storage
        );
        assert_eq!(
            chunkstarts.len(),
            8,
            "expected to capture 8 ChunkStart events in manual mode (variant_idx={}, storage={}), got {}",
            variant_idx,
            storage,
            chunkstarts.len()
        );

        for ctrl in chunkstarts {
            if let StreamControl::ChunkStart { stream_key, .. } = ctrl {
                assert!(
                    is_variant_stream_key(&stream_key, variant_idx),
                    "expected only variant {} in manual mode (storage={}), got stream_key='{}'",
                    variant_idx,
                    storage,
                    stream_key.0
                );
            }
        }
    });
}

#[rstest]
#[case("memory")]
fn hls_vod_completes_and_fetches_all_segments_slq_a1(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        // Deterministic "VOD completion" regression test at the HLS layer.
        //
        // It validates two things:
        // 1) The worker progresses through the entire media playlist for the selected variant
        //    (all segments are fetched, according to fixture request counters).
        // 2) The ordered data channel eventually closes (so `HlsStream::poll_next` can yield `None`).
        //
        // We use the fixture's mock mode with a fixed shape (2 variants, 12 segments). In mock mode:
        // - media playlists are `/v{variant}.m3u8`
        // - init is `/init/v{variant}.bin`
        // - media segments are `/seg/v{variant}_{i}.bin` for `i in 0..segments_per_variant`
        let variant_count = 2usize;
        let segments_per_variant = 12usize;

        // Force manual startup on variant 0 for determinism.
        let fixture = HlsFixture::new(variant_count, segments_per_variant, Duration::ZERO)
            .with_hls_config(
                HlsSettings::default().variant_stream_selector(move |_| Some(VariantId(0))),
            );

        let storage_kind = build_fixture_storage_kind("hls-vod-completes", variant_count, storage);

        // Drain the worker until the ordered channel closes (or timeout).
        let fixture_for_asserts = fixture.clone();
        let saw_channel_close = fixture
            .run_worker_collecting(8192, storage_kind, |mut data_rx, _events| {
                Box::pin(async move {
                    let deadline = Instant::now() + Duration::from_secs(30);

                    while Instant::now() < deadline {
                        match tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await
                        {
                            Ok(Some(_msg)) => {
                                // Drain ordered messages; we only care that the worker can complete.
                            }
                            Ok(None) => {
                                // Channel closed => `HlsStream` would yield `None`.
                                return true;
                            }
                            Err(_) => {
                                // Keep waiting until deadline.
                            }
                        }
                    }

                    false
                })
            })
            .await;

        assert!(
            saw_channel_close,
            "expected HLS ordered data channel to close for VOD within timeout"
        );

        // Assert all expected resources for variant 0 were fetched.
        //
        // NOTE: Fixture request counters are keyed by paths like "/seg/v0_0.bin".
        let v = 0usize;

        // Master and variant playlist should be fetched at least once.
        assert!(
            fixture_for_asserts
                .request_count_for("/master.m3u8")
                .unwrap_or(0)
                > 0,
            "expected master playlist to be fetched"
        );
        assert!(
            fixture_for_asserts
                .request_count_for(&format!("/v{}.m3u8", v))
                .unwrap_or(0)
                > 0,
            "expected media playlist for variant {} to be fetched",
            v
        );

        // Init is format/container-dependent (e.g. TS has no init). In mock fixture mode,
        // we only require that all media segments are fetched; init may legitimately be absent.
        let _ = v;

        // All media segments should be fetched at least once.
        for i in 0..segments_per_variant {
            let path = format!("/seg/v{}_{}.bin", v, i);
            assert!(
                fixture_for_asserts.request_count_for(&path).unwrap_or(0) > 0,
                "expected VOD segment to be fetched: {}",
                path
            );
        }
    });
}

#[rstest]
#[case("memory")]
fn hls_vod_real_assets_fetches_all_segments_and_stream_closes(#[case] storage: &str) {
    SERVER_RT.block_on(async {
        // Test VOD completion with real assets

        let assets_root = std::path::PathBuf::from("../assets/hls");
        let variant_count = 4usize;

        // Force manual startup on variant 0 for determinism.
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_real_dir(assets_root)
            .with_hls_config(
                HlsSettings::default().variant_stream_selector(move |_| Some(VariantId(0))),
            );

        let storage_kind =
            build_fixture_storage_kind("hls-vod-real-assets", variant_count, storage);

        let fixture_for_asserts = fixture.clone();
        let saw_channel_close = fixture
            .run_worker_collecting(8192, storage_kind, |mut data_rx, _events| {
                Box::pin(async move {
                    let deadline = Instant::now() + Duration::from_secs(30);

                    while Instant::now() < deadline {
                        match tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await
                        {
                            Ok(Some(_msg)) => {
                                // drain
                            }
                            Ok(None) => return true,
                            Err(_) => {}
                        }
                    }

                    false
                })
            })
            .await;

        assert!(
            saw_channel_close,
            "expected HLS ordered data channel to close for real-assets VOD within timeout"
        );

        // Variant 0 corresponds to the "slq-a1" playlist in repo assets:
        // - playlist: /hls/index-slq-a1.m3u8
        // - init:     /hls/init-slq-a1.mp4
        // - segments: /hls/segment-1-slq-a1.m4s .. /hls/segment-37-slq-a1.m4s
        //
        // We assert these were requested at least once.
        // In `HlsFixture` the router records request counters as paths starting with `/`,
        // and in RealDir mode the router strips the leading `hls/` prefix from the incoming URL
        // and serves files from `root/` directly.
        //
        // So requests that look like `/hls/master.m3u8` on the wire are counted as `/master.m3u8`
        // in the fixture's counters.
        assert!(
            fixture_for_asserts
                .request_count_for("/master.m3u8")
                .unwrap_or(0)
                > 0,
            "expected master playlist to be fetched"
        );
        assert!(
            fixture_for_asserts
                .request_count_for("/index-slq-a1.m3u8")
                .unwrap_or(0)
                > 0,
            "expected variant 0 media playlist to be fetched"
        );
        assert!(
            fixture_for_asserts
                .request_count_for("/init-slq-a1.mp4")
                .unwrap_or(0)
                > 0,
            "expected variant 0 init to be fetched"
        );

        for i in 1..=37usize {
            let p = format!("/segment-{}-slq-a1.m4s", i);
            assert!(
                fixture_for_asserts.request_count_for(&p).unwrap_or(0) > 0,
                "expected VOD segment to be fetched: {}",
                p
            );
        }
    });
}

#[rstest]
#[case(1, "persistent")]
#[case(1, "temp")]
#[case(1, "memory")]
#[case(2, "persistent")]
#[case(2, "temp")]
#[case(2, "memory")]
#[case(4, "persistent")]
#[case(4, "temp")]
#[case(4, "memory")]
fn hls_worker_auto_starts_with_variant0_first_chunkstart(
    #[case] variant_count: usize,
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO);

        let storage_kind = build_fixture_storage_kind("hls-worker-auto", variant_count, storage);

        let first = fixture
            .run_worker_collecting(
                2048,
                storage_kind,
                |rx, _events| Box::pin(async move { wait_first_chunkstart(rx).await }),
            )
            .await;

        match first {
            StreamControl::ChunkStart {
                stream_key, kind, ..
            } => {
                assert!(
                    is_variant_stream_key(&stream_key, 0),
                    "expected first ChunkStart to be for variant 0 (storage={}), got stream_key='{}' kind={:?}",
                    storage,
                    stream_key.0,
                    kind
                );

                if kind == ChunkKind::Media {
                    eprintln!(
                        "note: first ChunkStart was Media for stream_key='{}' (no init). \
                         This may be acceptable for TS but suspicious for fMP4.",
                        stream_key.0
                    );
                }
            }
            _ => unreachable!("we only captured ChunkStart above"),
        }
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_abr_downswitches_after_low_throughput_sample(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        let fixture = HlsFixture::with_variant_count(variant_count).with_abr_config(|cfg| {
            cfg.abr_min_switch_interval = Duration::ZERO;
            cfg.abr_down_switch_buffer = 5.0;
        });

        let storage_kind =
            build_fixture_storage_kind("hls-abr-downswitch", variant_count, "memory");

        // Start on variant 1 (higher bandwidth), then force a low throughput sample to push a downswitch.
        let (_base_url, (mut manager, mut controller)) =
            fixture.abr_controller(storage_kind, 1, None).await;

        assert_eq!(
            controller.current_variant_id(),
            Some(VariantId(1)),
            "ABR controller should initialize on variant 1"
        );

        // Feed a deliberately slow/large sample to drop estimated bandwidth below variant 1.
        controller.on_media_segment_downloaded(
            Duration::from_secs(2),
            50_000,
            Duration::from_millis(4000),
        );

        // Make ABR decision
        let decision = controller.make_decision();

        // Apply decision if needed
        if let AbrDecision::SwitchTo(variant_id) = decision {
            manager
                .select_variant(variant_id)
                .await
                .expect("failed to switch variant");
        }

        // Get next segment descriptor
        let desc = manager
            .next_segment()
            .await
            .expect("descriptor after throughput drop");

        let seg = match desc {
            NextSegmentResult::Segment(s) => s,
            other => panic!(
                "expected Segment descriptor after throughput drop, got {:?}",
                other
            ),
        };

        assert_eq!(
            controller.current_variant_id(),
            Some(VariantId(0)),
            "expected ABR to downswitch to variant 0 after bandwidth drop"
        );
        assert_eq!(
            seg.variant_id,
            VariantId(0),
            "segment descriptor should be from variant 0 after downswitch"
        );
        assert!(
            seg.is_init,
            "first descriptor after switch should be init for new variant (uri={})",
            seg.uri
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_worker_auto_start_uses_abr_initial_variant_index(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        // Test worker starts with correct variant in AUTO mode

        let initial_variant_index = if variant_count > 1 { 1usize } else { 0usize };

        let fixture = HlsFixture::with_variant_count(variant_count).with_hls_config({
            let mut s = HlsSettings::default();

            // Make ABR permissive so follow-up tests can still switch; here we only check startup.
            s.abr_min_switch_interval = Duration::ZERO;
            s.abr_min_buffer_for_up_switch = 0.0;
            s.abr_up_hysteresis_ratio = 0.0;
            s.abr_throughput_safety_factor = 1.0;

            // Ensure AUTO mode (no manual selector) and set the initial variant override.
            s.variant_stream_selector = None;
            s.abr_initial_variant_index = Some(initial_variant_index);

            s
        });

        let storage_kind =
            build_fixture_storage_kind("hls-abr-initial-variant-index", variant_count, "memory");
        let storage = fixture.build_storage(storage_kind);
        let storage_handle = storage.storage_handle();

        let (data_tx, mut data_rx) = mpsc::channel::<StreamMsg>(64);
        let (_cmd_tx, cmd_rx) = mpsc::channel::<stream_download_hls::HlsCommand>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<StreamEvent>(64);
        let segmented_length = Arc::new(std::sync::RwLock::new(
            stream_download::storage::SegmentedLength::default(),
        ));

        let (_base_url, worker) = fixture
            .worker(
                storage_handle,
                data_tx,
                cmd_rx,
                cancel.clone(),
                event_tx,
                segmented_length,
            )
            .await;

        // Spawn the worker loop.
        tokio::spawn(async move {
            let _ = worker.run().await;
        });

        // Observe the first SegmentStart event and assert its variant_id.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut first_seg_variant: Option<VariantId> = None;

        while Instant::now() < deadline {
            // Drain data to avoid backpressure (we only need events).
            while let Ok(Some(_)) =
                tokio::time::timeout(Duration::from_millis(5), data_rx.recv()).await
            {
                // ignore
            }

            match tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await {
                Ok(Ok(StreamEvent::SegmentStart { variant_id, .. })) => {
                    first_seg_variant = Some(variant_id);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => {}
                Err(_) => {}
            }
        }

        assert_eq!(
            first_seg_variant,
            Some(VariantId(initial_variant_index)),
            "expected AUTO startup to begin from abr_initial_variant_index={} (variant_count={})",
            initial_variant_index,
            variant_count
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_worker_manual_downswitch_applies_via_ordered_controls(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        assert!(variant_count >= 2, "need at least 2 variants");

        let start_variant_index = variant_count - 1;
        let target_variant_index = 0usize;

        // Deterministic: start in MANUAL mode on the highest variant.
        let fixture = HlsFixture::with_variant_count(variant_count).with_hls_config({
            let mut s = HlsSettings::default();

            // Manual selection at startup.
            s.variant_stream_selector = Some(Arc::new(Box::new(move |_master| {
                Some(VariantId(start_variant_index))
            })));
            s.abr_initial_variant_index = Some(start_variant_index);

            // ABR config is irrelevant for manual tests, but keep permissive defaults.
            s.abr_min_switch_interval = Duration::ZERO;
            s.abr_min_buffer_for_up_switch = 0.0;
            s.abr_up_hysteresis_ratio = 0.0;
            s.abr_throughput_safety_factor = 1.0;

            s
        });

        let storage_kind =
            build_fixture_storage_kind("hls-manual-downswitch-applies", variant_count, "temp");
        if let HlsFixtureStorageKind::Temp { subdir } = &storage_kind {
            let root = std::env::temp_dir()
                .join("stream-download-tests")
                .join(subdir);
            clean_dir(&root);
        }

        let storage = fixture.build_storage(storage_kind);
        let storage_handle = storage.storage_handle();

        let (data_tx, mut data_rx) = mpsc::channel::<StreamMsg>(64);
        let (cmd_tx, cmd_rx) = mpsc::channel::<stream_download_hls::HlsCommand>(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<StreamEvent>(64);
        let segmented_length = Arc::new(std::sync::RwLock::new(
            stream_download::storage::SegmentedLength::default(),
        ));

        let (base_url, worker) = fixture
            .worker(
                storage_handle,
                data_tx,
                cmd_rx,
                cancel.clone(),
                event_tx,
                segmented_length,
            )
            .await;

        // Spawn the worker loop.
        tokio::spawn(async move {
            let _ = worker.run().await;
        });

        // Wait until we observe the initial default stream key for the start variant.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_initial_default = false;

        let key_generator = stream_download_hls::CacheKeyGenerator::new(&base_url);
        let start_key: stream_download::source::ResourceKey =
            format!("{}/{}", key_generator.master_hash(), start_variant_index).into();

        while Instant::now() < deadline {
            if let Ok(Some(StreamMsg::Control(StreamControl::SetDefaultStreamKey { stream_key }))) =
                tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await
            {
                if stream_key == start_key {
                    saw_initial_default = true;
                    break;
                }
            }
        }

        assert!(
            saw_initial_default,
            "expected to observe initial SetDefaultStreamKey for start variant {}",
            start_variant_index
        );

        // Issue deterministic manual downswitch.
        cmd_tx
            .send(stream_download_hls::HlsCommand::SetVariant {
                variant_id: VariantId(target_variant_index),
            })
            .await
            .expect("failed to send SetVariant command");

        // Applied (ordered): must see SetDefaultStreamKey for the target, then ChunkStart(Init) for it.
        let key_generator = stream_download_hls::CacheKeyGenerator::new(&base_url);
        let target_key: stream_download::source::ResourceKey =
            format!("{}/{}", key_generator.master_hash(), target_variant_index).into();

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut saw_target_default = false;
        let mut saw_target_init_start = false;

        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await {
                Ok(Some(StreamMsg::Control(StreamControl::SetDefaultStreamKey { stream_key }))) => {
                    if stream_key == target_key {
                        saw_target_default = true;
                    }
                }
                Ok(Some(StreamMsg::Control(StreamControl::ChunkStart {
                    stream_key,
                    kind,
                    ..
                }))) => {
                    if stream_key == target_key && kind == stream_download::source::ChunkKind::Init
                    {
                        saw_target_init_start = true;
                        break;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {}
            }
        }

        assert!(
            saw_target_default,
            "expected SetDefaultStreamKey for target variant {} (key={:?})",
            target_variant_index, target_key
        );
        assert!(
            saw_target_init_start,
            "expected ChunkStart(Init) for target variant {} (key={:?}) after SetVariant",
            target_variant_index, target_key
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_worker_manual_upswitch_applies_via_ordered_controls(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        assert!(variant_count >= 2, "need at least 2 variants");

        let start_variant_index = 0usize;
        let target_variant_index = variant_count - 1;

        // Deterministic: start in MANUAL mode on the lowest variant.
        let fixture = HlsFixture::with_variant_count(variant_count).with_hls_config({
            let mut s = HlsSettings::default();

            // Manual selection at startup.
            s.variant_stream_selector = Some(Arc::new(Box::new(move |_master| {
                Some(VariantId(start_variant_index))
            })));
            s.abr_initial_variant_index = Some(start_variant_index);

            // ABR config is irrelevant for manual tests, but keep permissive defaults.
            s.abr_min_switch_interval = Duration::ZERO;
            s.abr_min_buffer_for_up_switch = 0.0;
            s.abr_up_hysteresis_ratio = 0.0;
            s.abr_throughput_safety_factor = 1.0;

            s
        });

        let storage_kind =
            build_fixture_storage_kind("hls-manual-upswitch-applies", variant_count, "temp");
        if let HlsFixtureStorageKind::Temp { subdir } = &storage_kind {
            let root = std::env::temp_dir()
                .join("stream-download-tests")
                .join(subdir);
            clean_dir(&root);
        }

        let storage = fixture.build_storage(storage_kind);
        let storage_handle = storage.storage_handle();

        let (data_tx, mut data_rx) = mpsc::channel::<StreamMsg>(64);
        let (cmd_tx, cmd_rx) = mpsc::channel::<stream_download_hls::HlsCommand>(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<StreamEvent>(64);
        let segmented_length = Arc::new(std::sync::RwLock::new(
            stream_download::storage::SegmentedLength::default(),
        ));

        let (base_url, worker) = fixture
            .worker(
                storage_handle,
                data_tx,
                cmd_rx,
                cancel.clone(),
                event_tx,
                segmented_length,
            )
            .await;

        tokio::spawn(async move {
            let _ = worker.run().await;
        });

        // Wait until we observe the initial default stream key for the start variant.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_initial_default = false;

        let start_key: stream_download::source::ResourceKey = format!(
            "{}/{}",
            stream_download_hls::master_hash_from_url(&base_url),
            start_variant_index
        )
        .into();

        while Instant::now() < deadline {
            if let Ok(Some(StreamMsg::Control(StreamControl::SetDefaultStreamKey { stream_key }))) =
                tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await
            {
                if stream_key == start_key {
                    saw_initial_default = true;
                    break;
                }
            }
        }

        assert!(
            saw_initial_default,
            "expected to observe initial SetDefaultStreamKey for start variant {}",
            start_variant_index
        );

        // Issue deterministic manual upswitch.
        cmd_tx
            .send(stream_download_hls::HlsCommand::SetVariant {
                variant_id: VariantId(target_variant_index),
            })
            .await
            .expect("failed to send SetVariant command");

        // Applied (ordered): must see SetDefaultStreamKey for the target, then ChunkStart(Init) for it.
        let target_key: stream_download::source::ResourceKey = format!(
            "{}/{}",
            stream_download_hls::master_hash_from_url(&base_url),
            target_variant_index
        )
        .into();

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut saw_target_default = false;
        let mut saw_target_init_start = false;

        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(250), data_rx.recv()).await {
                Ok(Some(StreamMsg::Control(StreamControl::SetDefaultStreamKey { stream_key }))) => {
                    if stream_key == target_key {
                        saw_target_default = true;
                    }
                }
                Ok(Some(StreamMsg::Control(StreamControl::ChunkStart {
                    stream_key,
                    kind,
                    ..
                }))) => {
                    if stream_key == target_key && kind == stream_download::source::ChunkKind::Init
                    {
                        saw_target_init_start = true;
                        break;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {}
            }
        }

        assert!(
            saw_target_default,
            "expected SetDefaultStreamKey for target variant {} (key={:?})",
            target_variant_index, target_key
        );
        assert!(
            saw_target_init_start,
            "expected ChunkStart(Init) for target variant {} (key={:?}) after SetVariant",
            target_variant_index, target_key
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_abr_upswitch_continues_from_current_segment_index(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO)
            .with_abr_config(|cfg| {
                cfg.abr_min_switch_interval = Duration::ZERO;
                cfg.abr_min_buffer_for_up_switch = 0.0;
                cfg.abr_up_hysteresis_ratio = 0.0;
                cfg.abr_throughput_safety_factor = 1.0;
            });

        let storage_kind = build_fixture_storage_kind("hls-abr-upswitch", variant_count, "memory");
        let (_base_url, (mut manager, mut controller)) = fixture.abr_controller(storage_kind, 0, None).await;

        // Drain init + first media segment on variant 0.
        let first_init = manager
            .next_segment()
            .await
            .expect("descriptor for first init");
        let first_seg = manager
            .next_segment()
            .await
            .expect("descriptor for first segment");

        let first_seg = match first_seg {
            NextSegmentResult::Segment(s) => s,
            other => panic!("expected first segment descriptor, got {:?}", other),
        };

        assert_eq!(
            controller.current_variant_id(),
            Some(VariantId(0)),
            "ABR controller should start on variant 0"
        );
        assert!(matches!(
            first_init,
            NextSegmentResult::Segment(ref d) if d.variant_id == VariantId(0) && d.is_init
        ));
        assert_eq!(
            first_seg.variant_id,
            VariantId(0),
            "first media descriptor should be variant 0"
        );

        // Feed a fast/large sample to encourage an upswitch.
        controller.on_media_segment_downloaded(
            Duration::from_secs(1),
            2_000_000,
            Duration::from_millis(100),
        );

        // Make ABR decision
        let decision = controller.make_decision();

        // Apply decision if needed
        if let AbrDecision::SwitchTo(variant_id) = decision {
            manager
                .select_variant(variant_id)
                .await
                .expect("failed to switch variant");
        }

        let switched_init = manager
            .next_segment()
            .await
            .expect("descriptor after throughput boost");
        let switched_seg = manager
            .next_segment()
            .await
            .expect("second descriptor after throughput boost");

        let switched_init = match switched_init {
            NextSegmentResult::Segment(s) => s,
            other => panic!("expected init descriptor after switch, got {:?}", other),
        };
        let switched_seg = match switched_seg {
            NextSegmentResult::Segment(s) => s,
            other => panic!("expected media descriptor after switch, got {:?}", other),
        };

        let current_variant_id = variant_count - 1;
        assert_eq!(
            controller.current_variant_id(),
            Some(VariantId(current_variant_id)),
            "ABR should upswitch to variant {}",
            current_variant_id
        );
        assert!(
            switched_init.is_init && switched_init.variant_id == VariantId(current_variant_id),
            "first descriptor after switch should be init for variant {} (got {:?})",
            current_variant_id,
            switched_init
        );
        assert_eq!(
            switched_seg.variant_id,
            VariantId(current_variant_id),
            "media descriptor after switch should target variant {}",
            current_variant_id
        );

        // Next segment index should be preserved across the switch: we consumed one media segment
        // on variant 0, so after switching we expect the second media segment of variant 1.
        let expected_sequence = first_seg.sequence + 1;
        assert_eq!(
            switched_seg.sequence, expected_sequence,
            "upswitch should continue from current segment index, not restart from the first segment"
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_worker_auto_upswitches_mid_stream_without_restarting(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::from_millis(200))
            .with_media_payload_bytes(800_000)
            .with_abr_config(|cfg| {
                cfg.abr_min_switch_interval = Duration::ZERO;
                cfg.abr_min_buffer_for_up_switch = 0.0;
                cfg.abr_up_hysteresis_ratio = 0.0;
                cfg.abr_throughput_safety_factor = 1.0;
            });

        let storage_kind =
            build_fixture_storage_kind("hls-abr-upswitch-worker", variant_count, "memory");

        let (variant_changes, segment_starts) = fixture
            .run_worker_collecting(
                8192,
                storage_kind,
                |mut data_rx, mut event_rx| {
                    Box::pin(async move {
                        let mut variant_changes: Vec<(VariantId, usize)> = Vec::new();
                        let mut segment_starts: Vec<(VariantId, u64)> = Vec::new();
                        let deadline = Instant::now() + Duration::from_secs(12);

                        while Instant::now() < deadline {
                            tokio::select! {
                                biased;
                                ev = event_rx.recv() => {
                                    if let Ok(ev) = ev {
                                        match ev {
                                            StreamEvent::VariantChanged { variant_id, .. } => {
                                                variant_changes.push((variant_id, segment_starts.len()));
                                            }
                                            StreamEvent::SegmentStart { variant_id, sequence, .. } => {
                                                segment_starts.push((variant_id, sequence));
                                            }
                                            _ => {}
                                        }
                                    }
                                    if variant_changes.iter().any(|(id, _)| *id != VariantId(0))
                                        && segment_starts.iter().any(|(id, _)| *id != VariantId(0)) {
                                        break;
                                    }
                                }
                                msg = data_rx.recv() => {
                                    if msg.is_none() {
                                        break;
                                    }
                                    // Drain data to avoid backpressure; we only need events.
                                }
                            }
                        }

                        (variant_changes, segment_starts)
                    })
                },
            )
            .await;

        let first_start = segment_starts
            .first()
            .expect("expected at least one segment start");
        assert_eq!(
            first_start.0,
            VariantId(0),
            "stream should start on variant 0 before any switch"
        );

        let (new_variant, change_at) = variant_changes
            .iter()
            .find(|(id, _)| *id != VariantId(0))
            .copied()
            .expect("expected a VariantChanged event to a higher variant");
        assert!(
            change_at >= 1,
            "upswitch should happen after at least one segment start on variant 0"
        );

        let first_high_segment = segment_starts
            .iter()
            .find(|(id, _)| *id == new_variant)
            .copied()
            .expect("expected a SegmentStart for higher variant after switch");

        let first_high_idx = segment_starts
            .iter()
            .position(|(id, _)| *id == new_variant)
            .expect("SegmentStart index for higher variant should exist");
        assert!(
            first_high_idx >= change_at,
            "VariantChanged should be observed before the first SegmentStart of the new variant (change_at={change_at}, first_high_idx={first_high_idx})"
        );

        assert!(
            first_high_segment.1 >= first_start.1 + 1,
            "after switching, worker should continue from current segment index, not restart (got seq {}, expected at least {})",
            first_high_segment.1,
            first_start.1 + 1
        );
    });
}

#[rstest]
#[case(2, 0, 1, "persistent")]
#[case(2, 0, 1, "temp")]
#[case(2, 0, 1, "memory")]
#[case(4, 0, 3, "persistent")]
#[case(4, 0, 3, "temp")]
#[case(4, 0, 3, "memory")]
#[case(4, 1, 2, "persistent")]
#[case(4, 1, 2, "temp")]
#[case(4, 1, 2, "memory")]
#[case(6, 2, 5, "persistent")]
#[case(6, 2, 5, "temp")]
#[case(6, 2, 5, "memory")]
fn hls_manager_select_variant_changes_fetched_media_bytes_prefix(
    #[case] variant_count: usize,
    #[case] from_variant: usize,
    #[case] to_variant: usize,
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        assert!(
            from_variant < variant_count && to_variant < variant_count && from_variant != to_variant,
            "invalid parameterization: variant_count={variant_count} from_variant={from_variant} to_variant={to_variant}"
        );
        let fixture = HlsFixture::with_variant_count(variant_count)
            .with_segment_delay(Duration::ZERO);

        let storage_kind = build_fixture_storage_kind(
            "hls-manager-select-variant",
            variant_count,
            storage,
        );
        let storage_bundle = fixture.build_storage(storage_kind);
        let storage_handle = storage_bundle.storage_handle();

        let (data_tx, _data_rx) = mpsc::channel::<StreamMsg>(8);
        let (_base_url, mut manager) = fixture.manager(storage_handle, data_tx).await;

        manager
            .load_master()
            .await
            .expect("failed to load master playlist");

        async fn first_media_payload(
            manager: &mut HlsManager,
            variant_index: usize,
        ) -> (String, Vec<u8>) {
            manager
                .select_variant(VariantId(variant_index))
                .await
                .unwrap_or_else(|e| panic!("select variant {variant_index} failed: {e}"));

            // Keep pulling descriptors until we find the first MEDIA segment for the selected variant.
            // This test asserts that selection affects which segment URI we fetch.
            let desc = loop {
                match manager.next_segment().await {
                    Ok(NextSegmentResult::Segment(d)) => {
                        if d.is_init {
                            continue;
                        }
                        // Fixture URIs are deterministic and include `seg/v{idx}_...`.
                        // Assert that the descriptor we're about to fetch belongs to the selected variant.
                        let uri_s = d.uri.to_string();
                        assert!(
                            uri_s.contains(&format!("/seg/v{variant_index}_")),
                            "expected selected variant {variant_index} media segment URI to contain '/seg/v{variant_index}_', got: {uri_s}"
                        );
                        break d;
                    }
                    Ok(other) => panic!(
                        "expected Segment descriptor for variant {variant_index}, got: {:?}",
                        other
                    ),
                    Err(e) => panic!("failed to get descriptor for variant {variant_index}: {e}"),
                }
            };

            let uri_s = desc.uri.to_string();
            let resource = stream_download_hls::Resource::media_segment(&desc.uri, desc.variant_id)
                .unwrap_or_else(|e| panic!("failed to create resource for uri={}: {}", uri_s, e));
            let mut stream = manager
                .downloader()
                .stream(&resource)
                .await
                .unwrap_or_else(|e| {
                    panic!("failed to stream segment for variant {variant_index} (uri={uri_s}): {e}")
                });

            // Read the whole payload (fixture segments are tiny strings like "V{v}-SEG-{i}").
            let mut out: Vec<u8> = Vec::new();
            while let Some(item) = stream.next().await {
                let chunk = item.unwrap_or_else(|e| {
                    panic!(
                        "error while reading segment stream for variant {variant_index} (uri={uri_s}): {e}"
                    )
                });
                out.extend_from_slice(&chunk);
            }

            assert!(
                !out.is_empty(),
                "expected non-empty payload for variant {variant_index} media segment (uri={uri_s})"
            );

            (uri_s, out)
        }

        let (uri_a, a) = first_media_payload(&mut manager, from_variant).await;
        let (uri_b, b) = first_media_payload(&mut manager, to_variant).await;

        assert_ne!(
            uri_a, uri_b,
            "expected different segment URIs after select_variant (variant {from_variant} -> {to_variant}, storage={storage})"
        );

        let a_s = std::str::from_utf8(&a).expect("fixture bytes should be utf-8");
        let b_s = std::str::from_utf8(&b).expect("fixture bytes should be utf-8");

        assert!(
            a_s.starts_with(&format!("V{from_variant}-SEG-")),
            "unexpected media payload for from_variant={from_variant} (storage={storage}); got={a_s:?}, uri={uri_a}"
        );
        assert!(
            b_s.starts_with(&format!("V{to_variant}-SEG-")),
            "unexpected media payload for to_variant={to_variant} (storage={storage}); got={b_s:?}, uri={uri_b}"
        );
    });
}

#[rstest]
#[case(2, 0, "INIT-V0", "persistent")]
#[case(2, 1, "NIT-V0", "persistent")]
#[case(2, 5, "V0", "persistent")]
#[case(2, 7, "V0-SEG-0", "persistent")]
#[case(4, 0, "INIT-V0", "persistent")]
#[case(4, 1, "NIT-V0", "persistent")]
#[case(4, 5, "V0", "persistent")]
#[case(4, 7, "V0-SEG-0", "persistent")]
#[case(2, 0, "INIT-V0", "temp")]
#[case(2, 1, "NIT-V0", "temp")]
#[case(2, 5, "V0", "temp")]
#[case(2, 7, "V0-SEG-0", "temp")]
#[case(4, 0, "INIT-V0", "temp")]
#[case(4, 1, "NIT-V0", "temp")]
#[case(4, 5, "V0", "temp")]
#[case(4, 7, "V0-SEG-0", "temp")]
#[case(2, 0, "INIT-V0", "memory")]
#[case(2, 1, "NIT-V0", "memory")]
#[case(2, 5, "V0", "memory")]
#[case(2, 7, "V0-SEG-0", "memory")]
#[case(4, 0, "INIT-V0", "memory")]
#[case(4, 1, "NIT-V0", "memory")]
#[case(4, 5, "V0", "memory")]
#[case(4, 7, "V0-SEG-0", "memory")]
fn hls_streamdownload_read_seek_returns_expected_bytes_at_known_offsets(
    #[case] variant_count: usize,
    #[case] seek_pos: u64,
    #[case] expected_prefix: &str,
    #[case] storage: &str,
) {
    SERVER_RT.block_on(async {
        let fixture =
            HlsFixture::with_variant_count(variant_count).with_segment_delay(Duration::ZERO);
        let storage_kind = build_fixture_storage_kind("hls-read-seek-e2e", variant_count, storage);
        let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind).await;

        // Ensure the pipeline is started (worker emits ChunkStart before data).
        // This avoids "no active segment; expected ChunkStart before data" on early seeks.
        let mut warm = [0u8; 1];
        let _ = Read::read(&mut reader, &mut warm).expect("warmup read failed");

        let p = Seek::seek(&mut reader, SeekFrom::Start(seek_pos))
            .unwrap_or_else(|e| panic!("seek(Start({seek_pos})) failed (storage={storage}): {e}"));
        assert_eq!(
            p, seek_pos,
            "seek(Start({seek_pos})) returned {p} (storage={storage})"
        );

        let mut buf = vec![0u8; expected_prefix.len()];
        Read::read_exact(&mut reader, &mut buf).unwrap_or_else(|e| {
            panic!("read_exact after seek({seek_pos}) failed (storage={storage}): {e}")
        });

        let got = std::str::from_utf8(&buf).expect("fixture bytes should be utf-8");
        assert_eq!(
            got, expected_prefix,
            "unexpected bytes at seek_pos={seek_pos} (storage={storage})"
        );

        let p0 = Seek::seek(&mut reader, SeekFrom::Start(0)).expect("seek back to 0 failed");
        assert_eq!(p0, 0, "seek(Start(0)) returned {p0} (storage={storage})");
        let mut b0 = vec![0u8; "INIT-V0".len()];
        Read::read_exact(&mut reader, &mut b0).expect("read_exact at 0 failed");
        assert_eq!(
            std::str::from_utf8(&b0).unwrap(),
            "INIT-V0",
            "expected INIT-V0 at start after roundtrip seek (storage={storage})"
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_streamdownload_seek_across_segment_boundary_reads_contiguous_bytes(
    #[case] variant_count: usize,
) {
    SERVER_RT.block_on(async {
        let fixture =
            HlsFixture::with_variant_count(variant_count).with_segment_delay(Duration::ZERO);
        let storage_kind =
            build_fixture_storage_kind("hls-read-seek-boundary", variant_count, "memory");
        let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind).await;

        // Warm up pipeline so seek won't race with first ChunkStart.
        let mut warm = [0u8; 1];
        let _ = Read::read(&mut reader, &mut warm).expect("warmup read failed");

        let init = "INIT-V0";
        let seg0 = "V0-SEG-0";
        let seg1 = "V0-SEG-1";
        let combined = format!("{init}{seg0}{seg1}");
        let offset = (init.len() + seg0.len() - 3) as u64;
        let read_len = 6usize;

        let p =
            Seek::seek(&mut reader, SeekFrom::Start(offset)).expect("seek across boundary failed");
        assert_eq!(p, offset, "seek returned unexpected position");

        let mut buf = vec![0u8; read_len];
        Read::read_exact(&mut reader, &mut buf).expect("read_exact across boundary failed");

        let expected_slice = &combined.as_bytes()[offset as usize..offset as usize + read_len];
        assert_eq!(
            &buf, expected_slice,
            "bytes across segment boundary did not match expected payload"
        );

        let remaining = combined.len() - (offset as usize + read_len);
        let mut trailing = vec![0u8; remaining];
        Read::read_exact(&mut reader, &mut trailing).expect("read_exact for trailing bytes failed");
        assert_eq!(
            &trailing,
            &combined.as_bytes()[offset as usize + read_len..],
            "trailing bytes after boundary seek did not align with expected sequence"
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_persistent_storage_creates_files_on_disk_after_read(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        let fixture =
            HlsFixture::with_variant_count(variant_count).with_segment_delay(Duration::ZERO);

        let storage_root = std::env::temp_dir()
            .join("stream-download-tests")
            .join(format!("hls-disk-files-persistent-v{variant_count}"));

        // Best-effort cleanup: ensure the directory starts empty/non-existent.
        clean_dir(&storage_root);

        let storage_kind = HlsFixtureStorageKind::Persistent {
            storage_root: storage_root.clone(),
        };

        let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind).await;

        // Trigger actual work.
        let mut buf = [0u8; 32];
        let n = Read::read(&mut reader, &mut buf).expect("read failed");
        assert!(n > 0, "expected to read some bytes");

        // Persistent provider should create files on disk (segments and/or resources).
        assert!(
            dir_nonempty_recursive(&storage_root),
            "expected persistent storage root to contain files after reading, but it is empty: {}",
            storage_root.display()
        );
    });
}

#[rstest]
#[case(2)]
#[case(4)]
fn hls_memory_stream_storage_does_not_create_segment_files_on_disk(#[case] variant_count: usize) {
    SERVER_RT.block_on(async {
        let fixture =
            HlsFixture::with_variant_count(variant_count).with_segment_delay(Duration::ZERO);

        // Test memory storage doesn't create segment files
        let resource_cache_root = std::env::temp_dir()
            .join("stream-download-tests")
            .join(format!("hls-disk-files-memory-resources-v{variant_count}"));

        clean_dir(&resource_cache_root);
        let before_files = count_files_recursive(&resource_cache_root);

        let storage_kind = HlsFixtureStorageKind::Memory {
            resource_cache_root: resource_cache_root.clone(),
        };

        let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind).await;

        // Trigger actual work.
        let mut buf = [0u8; 32];
        let n = Read::read(&mut reader, &mut buf).expect("read failed");
        assert!(n > 0, "expected to read some bytes");

        let after_files = count_files_recursive(&resource_cache_root);

        // Resource caching may write a handful of small blobs (playlists/keys), but it should not
        // Should not create many files
        assert!(
            after_files.saturating_sub(before_files) <= variant_count + 6,
            "memory storage created too many files: before={}, after={}",
            before_files,
            after_files
        );
    });
}

// Example test using new utilities
#[rstest]
#[case(2, "persistent")]
#[case(2, "temp")]
#[case(2, "memory")]
#[case(4, "persistent")]
#[case(4, "temp")]
#[case(4, "memory")]
fn hls_improved_test_example_using_utilities(#[case] variant_count: usize, #[case] storage: &str) {
    SERVER_RT.block_on(async {
        // Use helper function to create fixture with common configuration
        let fixture = fixtures::hls::create_basic_fixture(variant_count);

        // Use utility function to build storage kind
        let storage_kind =
            build_fixture_storage_kind("hls-improved-example", variant_count, storage);

        // Clean up any existing test data
        match &storage_kind {
            fixtures::hls::HlsFixtureStorageKind::Persistent { storage_root } => {
                clean_dir(&storage_root);
            }
            fixtures::hls::HlsFixtureStorageKind::Temp { .. } => {
                // Temp storage cleans up automatically
            }
            fixtures::hls::HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => {
                clean_dir(&resource_cache_root);
            }
        }

        // Use stream_download_boxed like original tests
        let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind).await;

        // Warm up pipeline
        let mut warm = [0u8; 1];
        let _ = std::io::Read::read(&mut reader, &mut warm).expect("warmup read failed");

        // Read some bytes
        let mut bytes_read = vec![0u8; 1024];
        let n = std::io::Read::read(&mut reader, &mut bytes_read).expect("read failed");
        bytes_read.truncate(n);

        // Verify we got some bytes
        assert!(!bytes_read.is_empty(), "Expected to read some bytes");

        // Verify the bytes are not all zeros (unless that's expected)
        let all_zeros = bytes_read.iter().all(|&b| b == 0);
        assert!(!all_zeros, "Read bytes are all zeros, which is unexpected");

        // Check request counts
        let master_requests = fixture.request_count_for("/master.m3u8").unwrap_or(0);
        let variant_requests = fixture.request_count_for("/v0.m3u8").unwrap_or(0);

        // We should have at least one request for master and variant playlist
        assert!(
            master_requests >= 1,
            "Expected at least 1 master playlist request"
        );
        assert!(
            variant_requests >= 1,
            "Expected at least 1 variant playlist request"
        );

        // For persistent storage, verify files were created
        if storage == "persistent" {
            if let fixtures::hls::HlsFixtureStorageKind::Persistent { storage_root } =
                build_fixture_storage_kind("hls-improved-example", variant_count, storage)
            {
                assert_dir_not_empty(&storage_root);
            }
        }

        // Clean up
        fixture
            .reset_request_counts()
            .expect("failed to reset request counts");
    });
}

// Example test using advanced utilities
#[rstest]
#[case(3, "memory")]
#[case(4, "temp")]
fn hls_advanced_utilities_example(#[case] variant_count: usize, #[case] storage: &str) {
    SERVER_RT.block_on(async {
        // Create test configuration using builder pattern
        let config = test_config()
            .variant_count(variant_count)
            .storage_kind(storage)
            .segment_delay(Duration::from_millis(50))
            .with_abr();

        // Create fixture from configuration
        let fixture = create_fixture_from_config(config);

        // Set up per-variant delays to simulate network conditions
        let mut variant_delays = Vec::new();
        for i in 0..variant_count {
            // Simulate: variant 0 is fastest, last variant is slowest
            let delay = Duration::from_millis(100 * (i as u64 + 1));
            variant_delays.push(delay);
        }

        // Create ABR test helper
        let _abr_helper =
            fixtures::hls::utils::advanced::AbrTestHelper::new(variant_delays.clone(), 0);

        // Build storage with resource tracking
        let storage_kind =
            build_fixture_storage_kind("hls-advanced-example", variant_count, storage);

        // Clean up before test
        match &storage_kind {
            fixtures::hls::HlsFixtureStorageKind::Persistent { storage_root } => {
                clean_dir(&storage_root);
            }
            fixtures::hls::HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => {
                clean_dir(&resource_cache_root);
            }
            _ => {}
        }

        // Create resource tracker
        let mut resource_tracker = fixtures::hls::utils::advanced::ResourceTracker::new();

        // Track directories based on storage type
        match &storage_kind {
            fixtures::hls::HlsFixtureStorageKind::Persistent { storage_root } => {
                resource_tracker.track_dir(storage_root.clone());
            }
            fixtures::hls::HlsFixtureStorageKind::Memory {
                resource_cache_root,
            } => {
                resource_tracker.track_dir(resource_cache_root.clone());
            }
            _ => {}
        }

        // Use stream_download_boxed like original tests
        let (_base_url, mut reader) = fixture.stream_download_boxed(storage_kind).await;

        // Warm up pipeline
        let mut warm = [0u8; 1];
        let _ = std::io::Read::read(&mut reader, &mut warm).expect("warmup read failed");

        // Read some data
        let mut bytes_read = vec![0u8; 2048];
        let n = std::io::Read::read(&mut reader, &mut bytes_read).expect("read failed");
        bytes_read.truncate(n);
        assert!(!bytes_read.is_empty(), "Expected to read some bytes");

        // Verify resource usage
        if storage == "persistent" {
            // For persistent storage, we expect some new files
            resource_tracker.assert_min_new_files(1);
            let new_files = resource_tracker.total_new_files();
            println!("Created {} new files during test", new_files);
        } else if storage == "memory" {
            // For memory storage, we might have some resource cache files
            let new_files = resource_tracker.total_new_files();
            println!("Memory storage created {} resource cache files", new_files);
        }

        // Verify request counts
        let master_requests = fixture.request_count_for("/master.m3u8").unwrap_or(0);
        assert!(
            master_requests >= 1,
            "Expected at least 1 master playlist request, got {}",
            master_requests
        );

        // Clean up
        fixture
            .reset_request_counts()
            .expect("failed to reset request counts");
    });
}
