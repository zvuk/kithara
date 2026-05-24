//! E2E reproduction of the zvuk stage `/drm/` burst-probe failure
//! mode, verifying the `SizeProbeMethod::RangeGet` strategy.
//!
//! Live HTTP — ignored by default; run explicitly:
//!
//! ```text
//! KITHARA_DRM_STAGE_AUTH_TOKEN=... \
//!     cargo test -p kithara-app --test stage_drm_burst_e2e -- --ignored --nocapture
//! ```
//!
//! Walks the master playlist, picks one variant's media playlist,
//! then mirrors what `SizeEstimator::try_head_requests` does in
//! production for `SizeProbeMethod::RangeGet`: fires
//! `init + first N segments` as single-byte `GET Range: bytes=0-0`
//! requests, chunked by the same concurrency cap as production.
//!
//! Real `HEAD`s against `init-slq-a1.mp4` on this upstream are
//! dropped by a WAF (`peer closed connection without sending TLS
//! close_notify`) — that's why `range_get` exists.
//!
//! Temporary by design — pinned to a stage track that's likely to rot.

use futures::future::join_all;
use kithara_net::{Headers, HttpClient, NetOptions, RangeSpec};
use url::Url;

const TRACK_MASTER: &str = "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8";
const STAGE_UA: &str = "OpenPlay - com.zvooq.openplay/4.30.0 (iPhone; iOS 17.5; Scale/3.00)";
const ENV_AUTH: &str = "KITHARA_DRM_STAGE_AUTH_TOKEN";
const PROBE_BURST_SEGMENTS: usize = 32;
/// Mirrors `HlsConfig::head_estimation_concurrency` in production.
const PROBE_BURST_CONCURRENCY: usize = 8;

fn drm_headers(auth: &str) -> Headers {
    let mut h = Headers::new();
    h.insert("User-Agent", STAGE_UA);
    h.insert("X-Auth-Token", auth);
    h
}

fn stage_client() -> HttpClient {
    HttpClient::new(
        NetOptions::builder().is_insecure(true).build(),
        tokio_util::sync::CancellationToken::new(),
    )
}

fn pick_first_variant(master: &str) -> String {
    master
        .lines()
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .expect("master playlist must list at least one variant URI")
        .trim()
        .to_owned()
}

fn parse_media(media: &str) -> (Option<String>, Vec<String>) {
    let mut init: Option<String> = None;
    let mut segments = Vec::new();
    for line in media.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            for attr in rest.split(',') {
                if let Some(v) = attr.trim().strip_prefix("URI=") {
                    init = Some(v.trim_matches('"').to_owned());
                }
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            segments.push(line.to_owned());
        }
    }
    (init, segments)
}

/// Extract `total` from `Content-Range: bytes 0-0/TOTAL` — this is
/// what production's `SizeEstimator::content_length` reads for the
/// `RangeGet` probe.
fn total_from_range(headers: &Headers) -> Option<u64> {
    headers
        .get("content-range")
        .or_else(|| headers.get("Content-Range"))
        .and_then(|h| h.split('/').nth(1))
        .filter(|s| *s != "*")
        .and_then(|s| s.parse::<u64>().ok())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live HTTP; run with --ignored when KITHARA_DRM_STAGE_AUTH_TOKEN is set"]
async fn stage_drm_range_get_probe_returns_full_size_map() {
    let auth = std::env::var(ENV_AUTH)
        .unwrap_or_else(|_| panic!("{ENV_AUTH} must be set to a valid zvuk stage X-Auth-Token"));
    let client = stage_client();
    let headers = drm_headers(&auth);
    let master_url = Url::parse(TRACK_MASTER).expect("master URL");

    let master_bytes = client
        .get_bytes(master_url.clone(), Some(headers.clone()))
        .await
        .expect("fetch master playlist");
    let master = String::from_utf8(master_bytes.to_vec()).expect("master is UTF-8");
    eprintln!("--- master playlist ({} bytes) ---\n{master}", master.len());

    let media_rel = pick_first_variant(&master);
    let media_url = master_url.join(&media_rel).expect("resolve media URL");
    let media_bytes = client
        .get_bytes(media_url.clone(), Some(headers.clone()))
        .await
        .expect("fetch media playlist");
    let media = String::from_utf8(media_bytes.to_vec()).expect("media is UTF-8");
    let (init_rel, segments_rel) = parse_media(&media);
    let init_rel = init_rel.expect("DRM media playlist carries #EXT-X-MAP");

    let mut urls: Vec<Url> = Vec::with_capacity(1 + PROBE_BURST_SEGMENTS);
    urls.push(media_url.join(&init_rel).expect("resolve init URL"));
    for seg in segments_rel.iter().take(PROBE_BURST_SEGMENTS) {
        urls.push(media_url.join(seg).expect("resolve segment URL"));
    }
    eprintln!(
        "probing {} URLs against {} (chunks of {})",
        urls.len(),
        media_url,
        PROBE_BURST_CONCURRENCY
    );

    let mut results = Vec::with_capacity(urls.len());
    for chunk in urls.chunks(PROBE_BURST_CONCURRENCY) {
        let chunk_results = join_all(chunk.iter().map(|u| {
            let client = client.clone();
            let headers = headers.clone();
            async move {
                let r = client
                    .get_range(u.clone(), RangeSpec::new(0, Some(0)), Some(headers))
                    .await;
                (u, r)
            }
        }))
        .await;
        results.extend(chunk_results);
    }

    let mut failed = 0usize;
    let mut total_sum: u64 = 0;
    for (url, r) in &results {
        match r {
            Ok(stream) => {
                let total = total_from_range(&stream.headers).unwrap_or(0);
                total_sum += total;
                eprintln!("OK  total_from_range={total:>7} {url}");
            }
            Err(e) => {
                failed += 1;
                eprintln!("ERR {e} {url}");
            }
        }
    }
    assert_eq!(
        failed,
        0,
        "all {} range-get probes must succeed (none did with `HEAD`); {} failed",
        results.len(),
        failed
    );
    assert!(
        total_sum > 0,
        "Content-Range header must carry resource totals (got 0 sum across {} probes)",
        results.len()
    );
}
