use std::sync::Arc;

use kithara_assets::{AssetLayoutRegistry, AssetResource, AssetSource, AssetStore, StorageBackend};
use kithara_bufpool::testing::{TestPools, pools};
use kithara_file::File;
use kithara_hls::Hls;
use kithara_play::policy::{QueryIdentityLayout, QueryIdentityRule};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::kithara;
use url::Url;

fn url(value: &str) -> Url {
    Url::parse(value).expect("valid test URL")
}

fn source(value: &str, discriminator: Option<&str>) -> AssetSource {
    AssetSource::Remote {
        url: url(value),
        discriminator: discriminator.map(str::to_string),
    }
}

fn store_with_layout<T: 'static>(layout: QueryIdentityLayout) -> AssetStore<TestPools> {
    let layouts = AssetLayoutRegistry::default().with::<T>(Arc::new(layout));
    AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .layouts(layouts)
        .build()
}

#[kithara::test]
fn file_cache_root_uses_only_configured_query_identity() {
    let layout =
        QueryIdentityLayout::new([QueryIdentityRule::new(["media.example.com"], ["track_id"])]);
    let store = store_with_layout::<File<TestPools>>(layout);

    let first = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?track_id=first&expires=100",
            None,
        ))
        .expect("first scope");
    let same_track = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?track_id=first&expires=200",
            None,
        ))
        .expect("same-track scope");
    let other_track = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?track_id=second&expires=100",
            None,
        ))
        .expect("other-track scope");

    assert_eq!(first.asset_root(), same_track.asset_root());
    assert_ne!(first.asset_root(), other_track.asset_root());
}

#[kithara::test]
fn hls_resource_paths_keep_configured_identity_and_drop_signatures() {
    let layout = QueryIdentityLayout::new([QueryIdentityRule::new(
        ["*.example.com"],
        ["track_id", "part"],
    )]);
    let store = store_with_layout::<Hls<TestPools>>(layout);
    let scope = store
        .scope::<Hls<TestPools>>(&source(
            "https://stream.example.com/master.m3u8?track_id=alpha",
            None,
        ))
        .expect("HLS scope");

    let first = scope
        .key(&AssetResource::Url(url(
            "https://cdn.example.com/audio/segment.m4s?track_id=alpha&part=1&expires=100",
        )))
        .expect("first key");
    let same = scope
        .key(&AssetResource::Url(url(
            "https://cdn.example.com/audio/segment.m4s?expires=200&part=1&track_id=alpha",
        )))
        .expect("same key");
    let other = scope
        .key(&AssetResource::Url(url(
            "https://cdn.example.com/audio/segment.m4s?track_id=beta&part=1&expires=100",
        )))
        .expect("other key");

    assert_eq!(first.rel_path(), same.rel_path());
    assert_ne!(first.rel_path(), other.rel_path());
}

#[kithara::test]
fn query_identity_distinguishes_missing_empty_and_repeated_values() {
    let layout = QueryIdentityLayout::new([QueryIdentityRule::new(["*"], ["id", "part"])]);
    let store = store_with_layout::<File<TestPools>>(layout);

    let missing = store
        .scope::<File<TestPools>>(&source("https://media.example.com/audio.mp3", None))
        .expect("missing scope");
    let wrong_case = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?ID=alpha",
            None,
        ))
        .expect("wrong-case scope");
    let empty = store
        .scope::<File<TestPools>>(&source("https://media.example.com/audio.mp3?id=", None))
        .expect("empty scope");
    let single = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?id=alpha&part=1",
            None,
        ))
        .expect("single scope");
    let repeated = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?id=alpha&part=1&part=2",
            None,
        ))
        .expect("repeated scope");

    assert_eq!(missing.asset_root(), wrong_case.asset_root());
    assert_ne!(missing.asset_root(), empty.asset_root());
    assert_ne!(single.asset_root(), repeated.asset_root());
}

#[kithara::test]
fn exact_wildcard_and_all_rules_are_case_insensitive_and_ordered() {
    let layout = QueryIdentityLayout::new([
        QueryIdentityRule::new(["MEDIA.EXAMPLE.COM"], ["exact"]),
        QueryIdentityRule::new(["*.Example.COM"], ["subdomain"]),
        QueryIdentityRule::new(["*"], ["global"]),
    ]);
    let store = store_with_layout::<File<TestPools>>(layout);

    let exact_first = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?exact=1&subdomain=a&global=a",
            None,
        ))
        .expect("exact first");
    let exact_ignores_later = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?exact=1&subdomain=b&global=b",
            None,
        ))
        .expect("exact ignores later rules");
    let wildcard_first = store
        .scope::<File<TestPools>>(&source(
            "https://cdn.example.com/audio.mp3?subdomain=1&global=a",
            None,
        ))
        .expect("wildcard first");
    let wildcard_changed = store
        .scope::<File<TestPools>>(&source(
            "https://cdn.example.com/audio.mp3?subdomain=2&global=a",
            None,
        ))
        .expect("wildcard changed");
    let bare_all = store
        .scope::<File<TestPools>>(&source(
            "https://example.com/audio.mp3?subdomain=1&global=a",
            None,
        ))
        .expect("bare host uses all");
    let bare_all_changed = store
        .scope::<File<TestPools>>(&source(
            "https://example.com/audio.mp3?subdomain=1&global=b",
            None,
        ))
        .expect("bare host all changed");

    assert_eq!(exact_first.asset_root(), exact_ignores_later.asset_root());
    assert_ne!(wildcard_first.asset_root(), wildcard_changed.asset_root());
    assert_ne!(bare_all.asset_root(), bare_all_changed.asset_root());
}

#[kithara::test]
fn query_identity_combines_existing_discriminator() {
    let layout = QueryIdentityLayout::new([QueryIdentityRule::new(["media.example.com"], ["id"])]);
    let store = store_with_layout::<File<TestPools>>(layout);

    let first = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?id=alpha",
            Some("quality-a"),
        ))
        .expect("first scope");
    let other_identity = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?id=beta",
            Some("quality-a"),
        ))
        .expect("other identity");
    let other_discriminator = store
        .scope::<File<TestPools>>(&source(
            "https://media.example.com/audio.mp3?id=alpha",
            Some("quality-b"),
        ))
        .expect("other discriminator");

    assert_ne!(first.asset_root(), other_identity.asset_root());
    assert_ne!(first.asset_root(), other_discriminator.asset_root());
}

#[kithara::test]
fn matched_signature_only_urls_share_a_path_while_unmatched_urls_delegate() {
    let layout =
        QueryIdentityLayout::new([QueryIdentityRule::new(["*.example.com"], ["track_id"])]);
    let store = store_with_layout::<Hls<TestPools>>(layout);
    let scope = store
        .scope::<Hls<TestPools>>(&source(
            "https://stream.example.com/master.m3u8?track_id=alpha",
            None,
        ))
        .expect("HLS scope");

    let matched_signed = scope
        .key(&AssetResource::Url(url(
            "https://cdn.example.com/audio/segment.m4s?expires=100",
        )))
        .expect("matched signed key");
    let matched_plain = scope
        .key(&AssetResource::Url(url(
            "https://cdn.example.com/audio/segment.m4s",
        )))
        .expect("matched plain key");
    let unmatched_first = scope
        .key(&AssetResource::Url(url(
            "https://outside.test/audio/segment.m4s?expires=100",
        )))
        .expect("unmatched first key");
    let unmatched_second = scope
        .key(&AssetResource::Url(url(
            "https://outside.test/audio/segment.m4s?expires=200",
        )))
        .expect("unmatched second key");

    assert_eq!(matched_signed.rel_path(), matched_plain.rel_path());
    assert_ne!(unmatched_first.rel_path(), unmatched_second.rel_path());
}
