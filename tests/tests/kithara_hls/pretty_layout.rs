#![cfg(not(target_arch = "wasm32"))]

//! Behavior test for the pretty on-disk layout: a 2-variant FLAC/fMP4 HLS
//! stream must materialize as `master.m3u8`, flat `<label>.m3u8` variant
//! playlists next to the master, and `<label>/init.mp4`,
//! `<label>/seg_<seq:05>.m4s` under the cache dir. Labels come from the
//! unique variant-playlist URI stems (`v0`, `v1`). A second fully-cached
//! open must issue zero segment/init body fetches.

use std::{collections::BTreeSet, fs, path::Path, sync::Arc};

use kithara::{
    assets::{PrettyLayout, StoreOptions},
    audio::{Audio, AudioConfig, ReadOutcome},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::PcmPattern,
};
use kithara_platform::{CancelToken, time::Duration, tokio::task::spawn_blocking};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const VARIANT_COUNT: usize = 2;
const SEGMENTS: usize = 3;

fn collect_rel_files(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out
}

fn drain(mut audio: Audio<Stream<Hls>>) {
    let mut buf = vec![0.0f32; 8192];
    for _ in 0..1000 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => break,
            Ok(ReadOutcome::Pending { .. }) => break,
            Err(e) => panic!("decode error: {e}"),
        }
    }
}

fn open_audio(url: url::Url, cache_dir: &Path) -> AudioConfig<Hls> {
    let store = StoreOptions::builder()
        .cache_dir(cache_dir.to_path_buf())
        .layout(Arc::new(PrettyLayout))
        .build();
    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .cancel(CancelToken::never())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(MediaInfo::new(
            Some(AudioCodec::Flac),
            Some(ContainerFormat::Fmp4),
        ))
        .build()
}

#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn pretty_layout_materializes_named_tree_and_second_open_hits_cache() {
    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(VARIANT_COUNT)
                .segments_per_variant(SEGMENTS)
                .packaged_audio_per_variant_pcm_flac(
                    SAMPLE_RATE,
                    CHANNELS,
                    vec![PcmPattern::Ascending, PcmPattern::Ascending],
                ),
        )
        .await
        .expect("create FLAC/fMP4 HLS fixture");
    let url = created.master_url();
    let token = created.token().to_owned();

    let init_gate = helper.register_init_gate(&token, 0);
    let seg_gates: Vec<_> = (0..SEGMENTS)
        .map(|s| helper.register_segment_gate(&token, 0, s))
        .collect();
    init_gate.release();
    for gate in &seg_gates {
        gate.release();
    }

    let temp_dir = TestTempDir::new();
    let cache_dir = temp_dir.path().to_path_buf();

    let audio = Audio::<Stream<Hls>>::new(open_audio(url.clone(), &cache_dir))
        .await
        .expect("first open");
    spawn_blocking({
        let audio = audio;
        move || drain(audio)
    })
    .await
    .expect("drain first open");

    let files = collect_rel_files(&cache_dir);
    // Exactly one asset root directory (ignore the store's `_index`).
    let root = files
        .iter()
        .filter_map(|f| f.split_once('/').map(|(root, _)| root.to_string()))
        .find(|r| r != "_index")
        .expect("at least one nested asset cache file");
    let rel: BTreeSet<String> = files
        .iter()
        .filter_map(|f| f.strip_prefix(&format!("{root}/")).map(str::to_string))
        .collect();

    for expected in [
        "master.m3u8",
        "v0.m3u8",
        "v1.m3u8",
        "v0/init.mp4",
        "v0/seg_00000.m4s",
        "v0/seg_00001.m4s",
        "v0/seg_00002.m4s",
    ] {
        assert!(
            rel.contains(expected),
            "pretty layout must materialize {expected}; got {rel:?}"
        );
    }

    let init_gets_after_first = init_gate.requested();
    let seg_gets_after_first: u64 = seg_gates.iter().map(|g| g.requested()).sum();
    assert!(
        init_gets_after_first >= 1 && seg_gets_after_first >= SEGMENTS as u64,
        "first open must fetch init + {SEGMENTS} segments (init={init_gets_after_first}, seg={seg_gets_after_first})"
    );

    let audio2 = Audio::<Stream<Hls>>::new(open_audio(url, &cache_dir))
        .await
        .expect("second open");
    spawn_blocking({
        let audio2 = audio2;
        move || drain(audio2)
    })
    .await
    .expect("drain second open");

    assert_eq!(
        init_gate.requested(),
        init_gets_after_first,
        "second open must not re-fetch the init segment"
    );
    assert_eq!(
        seg_gates.iter().map(|g| g.requested()).sum::<u64>(),
        seg_gets_after_first,
        "second open must not re-fetch any media segment"
    );
}
