#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Seek, SeekFrom};

use kithara::{
    assets::{AssetStore, StorageBackend},
    hls::{AbrMode, Hls, HlsConfig, SizeProbeMethod},
    platform::{time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    bufpool_ext::{TestPools, pools},
    hls_fixture::create_test_downloader,
};

#[kithara::test(tokio, timeout(Duration::from_secs(2)), hang_timeout_secs(1))]
async fn size_probe_resolves_exact_offsets() {
    let helper = TestServerHelper::new().await;
    let hls = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segment_size(50_000)
                .segments_per_variant(8),
        )
        .await
        .expect("create HLS fixture");
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let config = HlsConfig::for_url(hls.master_url())
        .downloader(create_test_downloader())
        .store(store)
        .pools(pools)
        .initial_abr_mode(AbrMode::manual(0))
        .size_probe_method(SizeProbeMethod::RangeGet)
        .build();
    let mut stream = Stream::<Hls<TestPools>>::new(config)
        .await
        .expect("create stream");
    let fixture = hls.clone();
    spawn_blocking(move || {
        let total = fixture.total_bytes();
        for offset in [total / 2 + 17, total - 64] {
            assert_eq!(stream.seek(SeekFrom::Start(offset)).expect("seek"), offset);
            let mut bytes = [0; 64];
            stream.read_exact(&mut bytes).expect("read probe");
            for (index, byte) in bytes.into_iter().enumerate() {
                assert_eq!(byte, fixture.expected_byte_at(0, offset + index as u64));
            }
        }
        assert_eq!(stream.read(&mut [0]).expect("read EOF"), 0);
    })
    .await
    .expect("blocking reader");
    assert!(
        helper.total_size_probe_count(&hls) > 0,
        "range size probe must run"
    );
}
