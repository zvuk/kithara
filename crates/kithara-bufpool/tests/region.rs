use kithara_bufpool::{BudgetExhausted, Region, RegionConfig};
use kithara_test_utils::kithara;

#[kithara::test]
fn byte_growth_exhausts_shared_budget_for_pcm() {
    let region = Region::new(RegionConfig::default().max_bytes(16));
    let byte_pool = region.byte_pool();
    let pcm_pool = region.pcm_pool();

    let mut bytes = byte_pool.get();
    bytes.ensure_len(16).unwrap();

    let mut pcm = pcm_pool.get();
    assert_eq!(pcm.ensure_len(1), Err(BudgetExhausted));
}

#[kithara::test]
fn pcm_growth_exhausts_shared_budget_for_bytes() {
    let region = Region::new(RegionConfig::default().max_bytes(16));
    let byte_pool = region.byte_pool();
    let pcm_pool = region.pcm_pool();

    let mut pcm = pcm_pool.get();
    pcm.ensure_len(4).unwrap();

    let mut bytes = byte_pool.get();
    assert_eq!(bytes.ensure_len(1), Err(BudgetExhausted));
}

/// Default PCM policy stores 128 buffers across 8 shards, so one home shard
/// queue holds 16. A single-threaded test always returns to the same home
/// shard, which makes the 17th return deterministically overflow the queue.
const PCM_SHARD_SLOTS: usize = 16;

/// Bytes charged for one `ensure_len(4)` PCM buffer (4 × f32).
const PCM_BUF_BYTES: usize = 16;

#[kithara::test]
fn rejected_drop_releases_shared_budget() {
    let region = Region::new(RegionConfig::default());
    let pcm_pool = region.pcm_pool();

    let held: Vec<_> = (0..PCM_SHARD_SLOTS + 1)
        .map(|_| {
            let mut buf = pcm_pool.get();
            buf.ensure_len(4).unwrap();
            buf
        })
        .collect();
    assert_eq!(
        region.stats().allocated_bytes,
        (PCM_SHARD_SLOTS + 1) * PCM_BUF_BYTES
    );
    drop(held);

    assert_eq!(
        region.stats().allocated_bytes,
        PCM_SHARD_SLOTS * PCM_BUF_BYTES
    );
}

#[kithara::test]
fn rejected_recycle_releases_shared_budget() {
    let region = Region::new(RegionConfig::default());
    let pcm_pool = region.pcm_pool();

    let mut extra = pcm_pool.get();
    extra.ensure_len(4).unwrap();
    let extra = extra.into_inner();

    let held: Vec<_> = (0..PCM_SHARD_SLOTS)
        .map(|_| {
            let mut buf = pcm_pool.get();
            buf.ensure_len(4).unwrap();
            buf
        })
        .collect();
    drop(held);
    pcm_pool.recycle(extra);

    assert_eq!(
        region.stats().allocated_bytes,
        PCM_SHARD_SLOTS * PCM_BUF_BYTES
    );
}

#[kithara::test]
fn stats_combine_budget_and_keep_pool_hits_separate() {
    let region = Region::new(RegionConfig::default().max_bytes(64));
    let byte_pool = region.byte_pool();
    let pcm_pool = region.pcm_pool();

    {
        let mut bytes = byte_pool.get();
        bytes.ensure_len(16).unwrap();
    }
    drop(byte_pool.get());
    {
        let mut pcm = pcm_pool.get();
        pcm.ensure_len(4).unwrap();
    }
    drop(pcm_pool.get());

    let stats = region.stats();
    assert_eq!(stats.allocated_bytes, 32);
    assert_eq!(stats.max_bytes, 64);
    assert_eq!(stats.byte_pool_hits, 1);
    assert_eq!(stats.byte_pool_misses, 1);
    assert_eq!(stats.pcm_pool_hits, 1);
    assert_eq!(stats.pcm_pool_misses, 1);
}
