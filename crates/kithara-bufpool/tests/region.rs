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

#[kithara::test]
fn failed_growth_preserves_buffer_and_budget() {
    let region = Region::new(RegionConfig::default().max_bytes(4));
    let byte_pool = region.byte_pool();
    let mut bytes = byte_pool.get();

    bytes.ensure_len(4).unwrap();
    bytes.copy_from_slice(&[1, 2, 3, 4]);
    let allocated = region.stats().allocated_bytes;

    assert_eq!(bytes.ensure_len(5), Err(BudgetExhausted));
    assert_eq!(&bytes[..], &[1, 2, 3, 4]);
    assert_eq!(region.stats().allocated_bytes, allocated);

    bytes.clear();
    bytes.ensure_len(4).unwrap();
    assert_eq!(bytes.len(), 4);
}

#[kithara::test]
fn successful_growth_charges_actual_capacity() {
    let region = Region::new(RegionConfig::default().max_bytes(1024));
    let pcm_pool = region.pcm_pool();
    let mut pcm = pcm_pool.get();

    pcm.ensure_len(17).unwrap();

    assert_eq!(
        region.stats().allocated_bytes,
        pcm.capacity() * size_of::<f32>()
    );
}

#[kithara::test]
fn attach_round_trip_keeps_travelling_charge_balanced() {
    let region = Region::new(RegionConfig::default().max_bytes(1024));
    let pcm_pool = region.pcm_pool();

    let mut pcm = pcm_pool.get();
    pcm.ensure_len(4).unwrap();
    let charged = region.stats().allocated_bytes;

    let inner = pcm.into_inner();
    assert_eq!(region.stats().allocated_bytes, charged);
    let reattached = pcm_pool.attach(inner);
    assert_eq!(region.stats().allocated_bytes, charged);
    drop(reattached);

    assert_eq!(region.stats().allocated_bytes, charged);
}

#[kithara::test]
fn smaller_ensure_len_keeps_capacity_charged() {
    let region = Region::new(RegionConfig::default().max_bytes(1024));
    let pcm_pool = region.pcm_pool();
    let mut pcm = pcm_pool.get();

    pcm.ensure_len(17).unwrap();
    let capacity = pcm.capacity();
    let allocated = region.stats().allocated_bytes;
    pcm.ensure_len(4).unwrap();

    assert_eq!(pcm.len(), 17);
    assert_eq!(pcm.capacity(), capacity);
    assert_eq!(region.stats().allocated_bytes, allocated);
}

#[kithara::test]
fn get_with_budget_overshoot_is_observable() {
    let region = Region::new(RegionConfig::default().max_bytes(1));
    let byte_pool = region.byte_pool();

    let bytes = byte_pool.get_with(|buf| buf.resize(2, 0));

    assert!(region.stats().allocated_bytes > 1);
    assert_eq!(region.stats().budget_overshoots, 1);
    assert_eq!(byte_pool.stats().budget_overshoots, 1);
    drop(bytes);
}

#[kithara::test]
fn growth_beyond_capacity_amortizes() {
    let region = Region::new(RegionConfig::default().max_bytes(1024));
    let pcm_pool = region.pcm_pool();
    let mut pcm = pcm_pool.get();

    pcm.ensure_len(16).unwrap();
    let first_cap = pcm.capacity();
    pcm.ensure_len(first_cap + 1).unwrap();
    assert!(pcm.capacity() >= first_cap * 2);

    let doubled = pcm.capacity();
    pcm.ensure_len(doubled).unwrap();
    assert_eq!(pcm.capacity(), doubled);
}

#[kithara::test]
fn amortized_growth_falls_back_to_exact_fit_under_budget() {
    let region = Region::new(RegionConfig::default().max_bytes(6));
    let byte_pool = region.byte_pool();
    let mut bytes = byte_pool.get();

    bytes.ensure_len(4).unwrap();
    bytes.ensure_len(6).unwrap();

    assert_eq!(bytes.capacity(), 6);
    assert_eq!(region.stats().allocated_bytes, 6);
}
