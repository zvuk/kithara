#![cfg(not(target_arch = "wasm32"))]

use hls_m3u8::{MasterPlaylist, MediaPlaylist, tags::VariantStream};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::hls::{
    HlsBundle, gapless_drm, gapless_plain, long_drm, long_plain, rss_plain,
};
use kithara_test_utils::kithara;

const LABELS: [&str; 4] = ["slq", "smq", "shq", "slossless"];
const SEGMENTS: usize = 37;
const SILVERCOMET: &str = "https://stream.silvercomet.top";

fn bytes<'a>(bundle: &'a HlsBundle, route: &str) -> Vec<u8> {
    let resource = bundle
        .get(route)
        .unwrap_or_else(|| panic!("bundle has no `{route}`"));
    std::fs::read(resource.path())
        .unwrap_or_else(|error| panic!("read {}: {error}", resource.path().display()))
}

fn text(bundle: &HlsBundle, route: &str) -> String {
    String::from_utf8(bytes(bundle, route))
        .unwrap_or_else(|error| panic!("`{route}` is not UTF-8: {error}"))
}

fn route(uri: &str) -> String {
    if uri.starts_with('/') {
        uri.to_owned()
    } else {
        format!("/hls/{uri}")
    }
}

fn variant_uri<'a>(stream: &'a VariantStream<'_>) -> &'a str {
    match stream {
        VariantStream::ExtXIFrame { uri, .. } | VariantStream::ExtXStreamInf { uri, .. } => uri,
    }
}

fn assert_route(bundle: &HlsBundle, uri: &str) {
    let route = route(uri);
    assert!(bundle.get(&route).is_some(), "bundle has no `{route}`");
}

fn assert_bundle(bundle: &HlsBundle, encrypted: bool, segments: usize) {
    let master_text = text(bundle, bundle.master_route());
    let master = MasterPlaylist::try_from(master_text.as_str()).expect("parse generated master");
    assert_eq!(master.variant_streams.len(), LABELS.len());

    for stream in &master.variant_streams {
        let uri = variant_uri(stream);
        assert_route(bundle, uri);
        let playlist_route = route(uri);
        let playlist_text = text(bundle, &playlist_route);
        let playlist = playlist_text
            .parse::<MediaPlaylist>()
            .expect("parse generated media playlist");
        assert!(playlist.has_end_list, "`{playlist_route}` must be VOD");
        assert_eq!(
            playlist.segments.values().count(),
            segments,
            "`{playlist_route}` segment count"
        );
        let mut maps = 0;
        let mut keys = 0;
        for segment in playlist.segments.values() {
            assert_route(bundle, segment.uri());
            if let Some(map) = &segment.map {
                maps += 1;
                assert_route(bundle, map.uri());
            }
            for key in segment.keys.iter().filter_map(|key| key.as_ref()) {
                keys += 1;
                assert_route(bundle, key.uri());
            }
        }
        assert_eq!(maps, 1, "`{playlist_route}` EXT-X-MAP count");
        assert_eq!(
            keys,
            segments * usize::from(encrypted),
            "`{playlist_route}` EXT-X-KEY count"
        );
    }

    for label in LABELS {
        assert_route(bundle, &format!("index-{label}-a1.m3u8"));
    }
}

fn live_text(path: &str) -> String {
    reqwest::blocking::get(format!("{SILVERCOMET}/{path}"))
        .and_then(reqwest::blocking::Response::error_for_status)
        .unwrap_or_else(|error| panic!("download Silvercomet `{path}`: {error}"))
        .text()
        .unwrap_or_else(|error| panic!("read Silvercomet `{path}`: {error}"))
}

fn assert_live_parity(bundle: &HlsBundle, live_root: &str, encrypted: bool) {
    let generated_master_text = text(bundle, bundle.master_route());
    let generated_master =
        MasterPlaylist::try_from(generated_master_text.as_str()).expect("parse generated master");
    let live_master_text = live_text(&format!("{live_root}/master.m3u8"));
    let live_master =
        MasterPlaylist::try_from(live_master_text.as_str()).expect("parse Silvercomet master");
    assert_eq!(
        generated_master.variant_streams,
        live_master.variant_streams
    );

    for stream in &generated_master.variant_streams {
        let uri = variant_uri(stream);
        let generated = text(bundle, &route(uri))
            .parse::<MediaPlaylist>()
            .unwrap_or_else(|error| panic!("parse generated `{uri}`: {error}"));
        let live = live_text(&format!("{live_root}/{uri}"))
            .parse::<MediaPlaylist>()
            .unwrap_or_else(|error| panic!("parse Silvercomet `{uri}`: {error}"));

        assert_eq!(generated.target_duration, live.target_duration, "{uri}");
        assert_eq!(generated.media_sequence, live.media_sequence, "{uri}");
        assert_eq!(generated.playlist_type, live.playlist_type, "{uri}");
        assert_eq!(generated.has_end_list, live.has_end_list, "{uri}");
        assert_eq!(
            generated.segments.values().count(),
            live.segments.values().count(),
            "{uri}"
        );

        let tolerance = if uri.contains("slossless") {
            0.11
        } else {
            0.03
        };
        for (generated, live) in generated.segments.values().zip(live.segments.values()) {
            assert_eq!(generated.uri(), live.uri(), "{uri}");
            assert_eq!(
                generated.map.as_ref().map(|map| map.uri()),
                live.map.as_ref().map(|map| map.uri()),
                "{uri} {} map",
                generated.uri()
            );
            assert_eq!(
                !generated.keys.is_empty(),
                encrypted,
                "{uri} {} encryption",
                generated.uri()
            );
            assert_eq!(
                !live.keys.is_empty(),
                encrypted,
                "{uri} {} live encryption",
                live.uri()
            );
            let generated_duration = generated.duration.duration().as_secs_f64();
            let live_duration = live.duration.duration().as_secs_f64();
            assert!(
                (generated_duration - live_duration).abs() < tolerance,
                "{uri} {} duration {generated_duration:.3} differs from Silvercomet {live_duration:.3}",
                generated.uri()
            );
        }
    }
}

#[kithara::test(native, flash(false))]
fn generated_plain_and_drm_bundles_are_complete() {
    let plain = long_plain();
    let drm = long_drm();

    assert_bundle(plain, false, SEGMENTS);
    assert_bundle(drm, true, SEGMENTS);

    let key = bytes(drm, "/hls/slq.key");
    assert_eq!(key.len(), 16);

    let plain_init = bytes(plain, "/hls/init-slq-a1.mp4");
    let encrypted_init = bytes(drm, "/hls/init-slq-a1.mp4");
    assert_ne!(encrypted_init, plain_init);

    let plain_media = bytes(plain, "/hls/segment-1-slq-a1.m4s");
    let encrypted_media = bytes(drm, "/hls/segment-1-slq-a1.m4s");
    assert_ne!(encrypted_media, plain_media);

    let mut decrypted = vec![0; encrypted_media.len()];
    let mut context = DecryptContext::new(key.try_into().expect("16-byte AES key"), [0; 16]);
    let written = aes128_cbc_process_chunk(&encrypted_media, &mut decrypted, &mut context, true)
        .expect("decrypt generated media");
    assert_eq!(&decrypted[..written], plain_media);
}

#[kithara::test(native, flash(false))]
#[ignore = "requires live Silvercomet HLS"]
fn generated_long_bundles_match_silvercomet_semantics() {
    assert_live_parity(long_plain(), "hls", false);
    assert_live_parity(long_drm(), "drm", true);
}

#[kithara::test(native, flash(false))]
fn generated_gapless_bundles_have_the_live_segment_topology() {
    for (bundle, encrypted) in [(gapless_plain(), false), (gapless_drm(), true)] {
        assert_bundle(bundle, encrypted, 9);

        for label in LABELS {
            let tolerance = if label == "slossless" { 0.11 } else { 0.03 };
            let playlist = text(bundle, &format!("/hls/index-{label}-a1.m3u8"))
                .parse::<MediaPlaylist>()
                .expect("parse generated gapless playlist");
            let durations = playlist
                .segments
                .values()
                .map(|segment| segment.duration.duration().as_secs_f64())
                .collect::<Vec<_>>();
            assert_eq!(durations.len(), 9);
            for (actual, expected) in durations
                .iter()
                .zip([4.0, 4.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0])
            {
                assert!(
                    (actual - expected).abs() < tolerance,
                    "{label} segment duration {actual:.3} differs from {expected:.3}"
                );
            }
            assert!(
                (3.0..4.0).contains(&durations[8]),
                "{label} tail duration must mirror the short live tail: {:.3}",
                durations[8]
            );
        }
    }
}

#[kithara::test(native, flash(false))]
fn generated_rss_bundle_keeps_the_measured_workload() {
    assert_bundle(rss_plain(), false, 25);
}
