#![cfg(not(target_arch = "wasm32"))]

//! A `<segment>.tmp` left behind by a dead writer must not brick the segment.
//!
//! Field incident (2026-08-20): four 1 MiB `.tmp` orphans from the previous
//! day sat next to the committed segments of one variant. Playback crossed the
//! three committed segments and died at the first orphaned one, permanently,
//! across app restarts — no error, no event, no EOF, just silence mid-track.
//!
//! Two defects met there, and one test each pins them.
//!
//! `AtomicChunked::open` claimed the tmp with `OpenOptions::create_new`, so an
//! orphan from a process that is long gone was indistinguishable from a live
//! sibling writer and every open returned `StorageError::TmpClaimed` forever.
//! The claim is now an exclusive advisory lock, which the OS releases on
//! process death.
//!
//! HLS then turned that permanent error into silence: `emit_fetch_cmd` logged
//! every acquire failure at DEBUG as "variant switch in flight", reverted the
//! slot to `Missing`, and `dispatch_from` pushed the fetch back on the queue —
//! so the slot stayed `planned`, `range_wait_phase` kept reporting
//! `WaitingDemand`, `range_has_failed` stayed false, and the decode gate parked
//! for good. Only a live-writer `TmpClaimed` requeues now; anything else
//! settles as failed.
use std::{
    fs,
    io::{self, Read},
    num::NonZeroUsize,
    path::PathBuf,
};

use kithara::{
    assets::{AssetResource, AssetSource, AssetStore, StorageBackend},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    bufpool_ext::{TestPools, pools},
    hls_server::{HlsTestServer, HlsTestServerConfig},
};

struct Consts;
impl Consts {
    const SEGMENT_SIZE: usize = 50_000;
    const SEGMENT_COUNT: usize = 5;
    /// Mid-playlist, so playback has to cross committed segments first and the
    /// stall lands where a listener hears it: in the middle of the track.
    const STALE_SEGMENT: usize = 3;
    const READ_CHUNK: usize = 8 * 1024;
}

#[kithara::test(tokio, serial, timeout(Duration::from_secs(15)), hang_timeout_secs(1))]
async fn stale_tmp_from_a_dead_writer_does_not_brick_a_segment() {
    let fixture = Fixture::new().await;
    let orphan = fixture.tmp_path_of_stale_segment();
    fs::create_dir_all(orphan.parent().expect("orphan parent")).expect("create segment directory");
    fs::write(&orphan, []).expect("plant orphan tmp");

    let read_bytes = fixture
        .read_to_eof()
        .await
        .expect("an orphan tmp left by a dead writer must not stop playback");

    assert_eq!(
        read_bytes,
        fixture.server.total_bytes(),
        "playback stopped short at segment {}",
        Consts::STALE_SEGMENT
    );
    assert!(
        fixture.canonical.is_file(),
        "segment {} never landed at {}",
        Consts::STALE_SEGMENT,
        fixture.canonical.display()
    );
    assert!(
        !orphan.exists(),
        "the planted orphan {} survived, so the store never claimed that path",
        orphan.display()
    );
}

/// An acquire failure the fetch can never recover from has to settle the slot
/// as failed. Reverting it to `Missing` re-plans the fetch, and that requeue is
/// invisible to the reader: the slot stays planned, so `range_wait_phase`
/// answers `WaitingDemand` while `range_has_failed` stays false, and the decode
/// gate parks with no error to report.
///
/// A directory at the tmp path is the cheapest permanent acquire failure that
/// needs no fault injection: the claim's `open` can never succeed on it.
#[kithara::test(tokio, serial, timeout(Duration::from_secs(15)), hang_timeout_secs(1))]
async fn a_segment_that_can_never_be_acquired_fails_the_read() {
    let fixture = Fixture::new().await;
    let blocked = fixture.tmp_path_of_stale_segment();
    fs::create_dir_all(&blocked).expect("block the tmp path with a directory");

    let err = fixture
        .read_to_eof()
        .await
        .expect_err("an unacquirable segment must fail the read, not park it");

    assert!(
        err.to_string().contains("segment data not ready"),
        "expected the terminal SegmentUnavailable, got: {err}"
    );
}

/// Server, store, and the on-disk path of [`Consts::STALE_SEGMENT`].
struct Fixture {
    server: HlsTestServer,
    /// The path the store commits the stale segment to. Derived through the
    /// store's own scope and key so a test cannot plant its tmp somewhere the
    /// store never looks.
    canonical: PathBuf,
    config: HlsConfig<TestPools>,
    _temp_dir: TestTempDir,
}

impl Fixture {
    async fn new() -> Self {
        let temp_dir = TestTempDir::new();
        let server = HlsTestServer::new(HlsTestServerConfig {
            segment_size: Consts::SEGMENT_SIZE,
            segments_per_variant: Consts::SEGMENT_COUNT,
            ..Default::default()
        })
        .await;
        let master_url = server.url("/master.m3u8");
        let stale_url = server.url(&format!("/seg/v0_{}.bin", Consts::STALE_SEGMENT));

        let root = temp_dir.path().to_path_buf();
        let pools = pools();
        let store = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk { root: root.clone() })
            .cache_capacity(NonZeroUsize::new(256).unwrap())
            .build();
        let key = store
            .scope::<Hls<TestPools>>(&AssetSource::Remote {
                url: master_url.clone(),
                discriminator: None,
            })
            .expect("hls asset scope")
            .key(&AssetResource::Url(stale_url))
            .expect("stale segment key");
        let canonical = root
            .join(key.asset_root().expect("relative asset root"))
            .join(key.rel_path().expect("relative resource path"));

        let config = HlsConfig::for_url(master_url)
            .store(store)
            .pools(pools)
            .cancel(CancelToken::never())
            .initial_abr_mode(AbrMode::manual(0))
            .build();
        Self {
            server,
            canonical,
            config,
            _temp_dir: temp_dir,
        }
    }

    /// Read the whole stream byte-for-byte, returning the byte count.
    async fn read_to_eof(&self) -> io::Result<u64> {
        let mut stream = Stream::<Hls<TestPools>>::new(self.config.clone())
            .await
            .expect("create stream");
        spawn_blocking(move || {
            let mut buf = vec![0u8; Consts::READ_CHUNK];
            let mut total = 0u64;
            loop {
                let n = stream.read(&mut buf)?;
                if n == 0 {
                    return Ok(total);
                }
                total += n as u64;
            }
        })
        .await
        .expect("blocking read task")
    }

    /// The temp-file companion path `AtomicChunked` claims for the stale
    /// segment: the canonical file *name* plus `.tmp`, sibling in the same
    /// directory.
    fn tmp_path_of_stale_segment(&self) -> PathBuf {
        let name = self
            .canonical
            .file_name()
            .expect("canonical file name")
            .to_str()
            .expect("utf-8 file name");
        self.canonical
            .parent()
            .expect("canonical parent")
            .join(format!("{name}.tmp"))
    }
}
