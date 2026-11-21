use std::{mem::size_of, sync::Barrier, thread};

use kithara_bufpool::{
    HasPool, OverallBudget, PoolAlias, PoolConfig, PoolError, PoolRegion, StringKey, VecKey,
    pool_schema,
};
use kithara_platform::sync::Arc;
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::{bufpool::TestPools, kithara};

fn config(max_buffers: usize) -> PoolConfig {
    PoolConfig::builder().max_buffers(max_buffers).build()
}

fn pools(max_bytes: usize) -> PoolRegion<TestPools> {
    TestPools::region(OverallBudget(max_bytes), config(usize::MAX), config(128))
        .unwrap_or_else(|error| panic!("test region: {error}"))
}

struct ForgedPools {
    bytes: kithara_bufpool::__private::PoolSlot<u8>,
}

impl HasPool<u8> for ForgedPools {
    fn __slot(&self) -> &kithara_bufpool::__private::PoolSlot<u8> {
        &self.bytes
    }
}

#[kithara::test]
#[should_panic(expected = "pool slot belongs to a different region")]
fn a_slot_cannot_escape_into_another_region() {
    let mut escaped = None;
    let _donor: PoolRegion<()> = PoolRegion::__build(OverallBudget(1024), |context| {
        escaped = Some(context.slot::<u8>(config(32))?);
        Ok(())
    })
    .unwrap_or_else(|error| panic!("donor region: {error}"));
    let forged = PoolRegion::__build(OverallBudget(1), |_| {
        Ok(ForgedPools {
            bytes: escaped.unwrap_or_else(|| panic!("slot escaped donor build")),
        })
    })
    .unwrap_or_else(|error| panic!("forged region: {error}"));

    let _ = forged.get_with_len::<u8>(2);
}

#[kithara::test]
fn direct_keys_return_nominal_checked_guards() {
    let pools = pools(1024);

    let bytes = pools
        .get_with_len::<u8>(4)
        .unwrap_or_else(|error| panic!("bytes: {error}"));
    let samples = pools
        .get_with_len::<f32>(4)
        .unwrap_or_else(|error| panic!("samples: {error}"));

    assert_eq!(&*bytes, &[0; 4]);
    assert_eq!(&*samples, &[0.0; 4]);
}

#[kithara::test]
fn byte_growth_exhausts_the_shared_budget_for_samples() {
    let pools = pools(16);
    let _bytes = pools
        .get_with_len::<u8>(16)
        .unwrap_or_else(|error| panic!("bytes: {error}"));

    assert!(matches!(
        pools.get_with_len::<f32>(1),
        Err(PoolError::OverallBudgetExceeded { .. })
    ));
}

#[kithara::test]
fn two_full_shares_still_compete_for_one_global_cap() {
    let pools = pools(16);
    let _samples = pools
        .get_with_len::<f32>(4)
        .unwrap_or_else(|error| panic!("samples: {error}"));

    assert!(matches!(
        pools.get_with_len::<u8>(1),
        Err(PoolError::OverallBudgetExceeded { .. })
    ));
}

#[kithara::test]
fn failed_growth_and_append_preserve_buffer_and_counters() {
    let pools = pools(4);
    let mut bytes = pools
        .get_with_len::<u8>(4)
        .unwrap_or_else(|error| panic!("bytes: {error}"));
    bytes.copy_from_slice(&[1, 2, 3, 4]);
    let capacity = bytes.capacity();
    let stats = pools.stats();

    assert!(bytes.ensure_len(5).is_err());
    assert!(bytes.try_extend_from_slice(&[5]).is_err());
    assert_eq!(&*bytes, &[1, 2, 3, 4]);
    assert_eq!(bytes.capacity(), capacity);
    assert_eq!(pools.stats().allocated_bytes, stats.allocated_bytes);
}

#[kithara::test]
fn incremental_growth_keeps_amortized_capacity() {
    let pools = pools(4096);
    let mut bytes = pools
        .get_with_len::<u8>(8)
        .unwrap_or_else(|error| panic!("bytes: {error}"));
    let initial_capacity = bytes.capacity();

    bytes
        .ensure_len(initial_capacity + 1)
        .unwrap_or_else(|error| panic!("grow bytes: {error}"));

    assert!(bytes.capacity() >= initial_capacity.saturating_mul(2));
}

#[kithara::test]
fn eager_payload_is_a_hit_on_another_thread() {
    let pools = TestPools::region(
        OverallBudget(1024),
        config(32),
        PoolConfig::builder()
            .initial_buffers(1)
            .initial_capacity(8)
            .max_buffers(128)
            .build(),
    )
    .unwrap_or_else(|error| panic!("test region: {error}"));
    let worker_pools = pools.clone();
    let initial_peak = pools.stats().peak_allocated_bytes;

    let buffer = thread::spawn(move || worker_pools.get_with_len::<f32>(8))
        .join()
        .unwrap_or_else(|_| panic!("worker panicked"))
        .unwrap_or_else(|error| panic!("samples: {error}"));

    assert!(buffer.capacity() >= 8);
    assert_eq!(pools.stats().peak_allocated_bytes, initial_peak);
    drop(buffer);
}

#[kithara::test]
fn zero_initial_buffers_allocate_no_payload_capacity() {
    let pools = pools(1024);

    assert_eq!(pools.stats().allocated_bytes, 0);
}

#[kithara::test]
fn invalid_configuration_is_rejected_before_publication() {
    let cases = [
        TestPools::region(
            OverallBudget(1024),
            PoolConfig::builder()
                .initial_buffers(9)
                .max_buffers(8)
                .build(),
            config(128),
        ),
        TestPools::region(
            OverallBudget(1024),
            config(32),
            PoolConfig::builder()
                .initial_buffers(9)
                .initial_capacity(1)
                .max_buffers(8)
                .build(),
        ),
        TestPools::region(
            OverallBudget(usize::MAX),
            config(32),
            PoolConfig::builder()
                .initial_buffers(1)
                .initial_capacity(usize::MAX)
                .max_buffers(128)
                .build(),
        ),
    ];

    assert!(
        cases
            .into_iter()
            .all(|result| matches!(result, Err(PoolError::InvalidConfig { .. })))
    );
}

mod aliases {
    use super::*;

    pub(super) enum FirstTag {}
    pub(super) enum SecondTag {}
    pub(super) type First = PoolAlias<FirstTag, f32>;
    pub(super) type Second = PoolAlias<SecondTag, f32>;

    pool_schema! {
        pub(super) AliasPools {
            first: First,
            second: Second,
        }
    }

    pub(super) fn pools() -> PoolRegion<AliasPools> {
        AliasPools::builder(OverallBudget(1024))
            .first(config(32))
            .second(config(32))
            .build()
            .unwrap_or_else(|error| panic!("alias region: {error}"))
    }
}

#[kithara::test]
fn aliases_of_one_item_type_own_distinct_physical_slots() {
    let pools = aliases::pools();
    let first = pools
        .get_with_len::<aliases::First>(4)
        .unwrap_or_else(|error| panic!("first: {error}"));
    let second = pools.get::<aliases::Second>();

    assert!(first.capacity() > 0);
    assert_eq!(second.capacity(), 0);
}

mod reclaim_keys {
    use super::*;

    pub(super) enum DonorTag {}
    pub(super) enum RequesterTag {}
    pub(super) type Donor = PoolAlias<DonorTag, VecKey<u8, 1>>;
    pub(super) type Requester = PoolAlias<RequesterTag, VecKey<u8, 1>>;

    pool_schema! {
        pub(super) ReclaimPools {
            donor: Donor,
            requester: Requester,
        }
    }

    pub(super) fn pools(max_bytes: usize) -> PoolRegion<ReclaimPools> {
        let config = || PoolConfig::builder().max_buffers(2).build();
        ReclaimPools::builder(OverallBudget(max_bytes))
            .donor(config())
            .requester(config())
            .build()
            .unwrap_or_else(|error| panic!("reclaim region: {error}"))
    }
}

#[kithara::test]
fn sibling_capacity_is_reclaimed_only_after_it_becomes_idle() {
    const BUDGET: usize = 8;

    let pools = reclaim_keys::pools(BUDGET);
    let first = pools
        .get_with_len::<reclaim_keys::Donor>(BUDGET / 2)
        .unwrap_or_else(|error| panic!("first donor: {error}"));
    let second = pools
        .get_with_len::<reclaim_keys::Donor>(BUDGET / 2)
        .unwrap_or_else(|error| panic!("second donor: {error}"));
    let mut requester = pools.get::<reclaim_keys::Requester>();

    assert!(matches!(
        requester.ensure_len(BUDGET),
        Err(PoolError::OverallBudgetExceeded { .. })
    ));
    drop(first);
    drop(second);
    assert_eq!(pools.stats().allocated_bytes, BUDGET);
    assert_eq!(pools.stats().peak_allocated_bytes, BUDGET);

    requester
        .ensure_len(BUDGET)
        .unwrap_or_else(|error| panic!("requester after idle reclaim: {error}"));

    assert_eq!(pools.stats().allocated_bytes, BUDGET);
    assert_eq!(pools.stats().peak_allocated_bytes, BUDGET);
    assert_eq!(pools.stats().max_bytes, BUDGET);
}

#[kithara::test]
fn own_idle_buffers_are_reclaimed_for_a_live_request_at_the_region_cap() {
    const BUFFER_BYTES: usize = 4;
    const POOL_BYTES: usize = 2 * BUFFER_BYTES;

    let pools = TestPools::region(
        OverallBudget(POOL_BYTES),
        PoolConfig::builder().max_buffers(64).build(),
        config(8),
    )
    .unwrap_or_else(|error| panic!("region-cap region: {error}"));
    let first = pools
        .get_with_len::<u8>(BUFFER_BYTES)
        .unwrap_or_else(|error| panic!("first buffer: {error}"));
    let second = pools
        .get_with_len::<u8>(BUFFER_BYTES)
        .unwrap_or_else(|error| panic!("second buffer: {error}"));
    drop(first);
    drop(second);
    let mut requester = pools.get::<u8>();
    assert_eq!(requester.capacity(), BUFFER_BYTES);

    requester
        .ensure_len(POOL_BYTES)
        .unwrap_or_else(|error| panic!("requester after own reclaim: {error}"));

    assert_eq!(requester.capacity(), POOL_BYTES);
    assert_eq!(pools.stats().allocated_bytes, POOL_BYTES);
}

#[kithara::test]
fn a_dropped_buffer_above_the_retention_ceiling_returns_its_whole_charge() {
    const RETAINED_CAPACITY: usize = 8;

    let pools = TestPools::region(
        OverallBudget(64),
        PoolConfig::builder()
            .max_buffers(32)
            .max_retained_capacity(RETAINED_CAPACITY)
            .build(),
        config(8),
    )
    .unwrap_or_else(|error| panic!("test region: {error}"));
    let _held = pools
        .get_with_len::<f32>(3)
        .unwrap_or_else(|error| panic!("held samples: {error}"));
    let baseline = pools.stats().allocated_bytes;

    drop(
        pools
            .get_with_len::<u8>(RETAINED_CAPACITY + 1)
            .unwrap_or_else(|error| panic!("oversized bytes: {error}")),
    );

    assert_eq!(pools.stats().allocated_bytes, baseline);
    assert_eq!(pools.get::<u8>().capacity(), 0);
}

#[kithara::test]
fn a_dropped_buffer_at_the_retention_ceiling_is_kept() {
    const RETAINED_CAPACITY: usize = 8;

    let pools = TestPools::region(
        OverallBudget(64),
        PoolConfig::builder()
            .max_buffers(32)
            .max_retained_capacity(RETAINED_CAPACITY)
            .build(),
        config(8),
    )
    .unwrap_or_else(|error| panic!("test region: {error}"));
    let bytes = pools
        .get_with_len::<u8>(RETAINED_CAPACITY)
        .unwrap_or_else(|error| panic!("bytes at the ceiling: {error}"));
    assert_eq!(bytes.capacity(), RETAINED_CAPACITY);
    let baseline = pools.stats().allocated_bytes;

    drop(bytes);

    assert_eq!(pools.get::<u8>().capacity(), RETAINED_CAPACITY);
    assert_eq!(pools.stats().allocated_bytes, baseline);
}

#[kithara::test]
fn a_refused_allocation_leaves_no_charge() {
    let pools = pools(usize::MAX);

    assert!(matches!(
        pools.get_with_len::<u8>(usize::MAX / 2 + 1),
        Err(PoolError::AllocationFailed { .. })
    ));
    assert_eq!(pools.stats().allocated_bytes, 0);
}

#[kithara::test]
fn growth_that_fits_the_remainder_succeeds_when_doubling_does_not() {
    let pools = pools(6);
    let mut bytes = pools
        .get_with_len::<u8>(4)
        .unwrap_or_else(|error| panic!("bytes: {error}"));
    bytes.copy_from_slice(&[1, 2, 3, 4]);
    assert_eq!(bytes.capacity(), 4);

    bytes
        .ensure_len(5)
        .unwrap_or_else(|error| panic!("growth within the remainder: {error}"));

    assert_eq!(&bytes[..4], &[1, 2, 3, 4]);
    assert_eq!(bytes.len(), 5);
}

#[kithara::test]
fn racing_typed_slots_never_admit_past_the_global_limit() {
    let pools = pools(64);
    let barrier = Arc::new(Barrier::new(3));

    let byte_pools = pools.clone();
    let byte_barrier = Arc::clone(&barrier);
    let bytes = thread::spawn(move || {
        byte_barrier.wait();
        byte_pools.get_with_len::<u8>(64)
    });

    let sample_pools = pools.clone();
    let sample_barrier = Arc::clone(&barrier);
    let samples = thread::spawn(move || {
        sample_barrier.wait();
        sample_pools.get_with_len::<f32>(64 / size_of::<f32>())
    });

    barrier.wait();
    let bytes = bytes
        .join()
        .unwrap_or_else(|_| panic!("byte worker panicked"));
    let samples = samples
        .join()
        .unwrap_or_else(|_| panic!("sample worker panicked"));

    assert_eq!(usize::from(bytes.is_ok()) + usize::from(samples.is_ok()), 1);
    assert!(pools.stats().allocated_bytes <= pools.stats().max_bytes);
}

#[kithara::test]
fn normalize_reuses_the_held_capacity() {
    const CAPACITY: usize = 8;

    let pools = TestPools::region(
        OverallBudget(4 * CAPACITY),
        PoolConfig::builder()
            .max_buffers(64)
            .max_retained_capacity(CAPACITY)
            .build(),
        config(8),
    )
    .unwrap_or_else(|error| panic!("test region: {error}"));
    let first = pools
        .get_with_len::<u8>(1)
        .unwrap_or_else(|error| panic!("first priming buffer: {error}"));
    let second = pools
        .get_with_len::<u8>(1)
        .unwrap_or_else(|error| panic!("second priming buffer: {error}"));
    drop(first);
    drop(second);
    let mut buffer = pools
        .get_with_len::<u8>(CAPACITY)
        .unwrap_or_else(|error| panic!("initial buffer: {error}"));
    let charged = pools.stats().allocated_bytes;

    buffer.normalize();
    buffer
        .ensure_len(CAPACITY)
        .unwrap_or_else(|error| panic!("renewed buffer: {error}"));

    assert_eq!(pools.stats().allocated_bytes, charged);
}

#[kithara::test]
fn normalize_drops_capacity_above_the_retention_ceiling() {
    const RETAINED_CAPACITY: usize = 8;

    let pools = TestPools::region(
        OverallBudget(2 * RETAINED_CAPACITY),
        PoolConfig::builder()
            .max_buffers(32)
            .max_retained_capacity(RETAINED_CAPACITY)
            .build(),
        config(8),
    )
    .unwrap_or_else(|error| panic!("test region: {error}"));
    let mut buffer = pools
        .get_with_len::<u8>(2 * RETAINED_CAPACITY)
        .unwrap_or_else(|error| panic!("oversized buffer: {error}"));
    assert!(pools.stats().allocated_bytes > 0);

    buffer.normalize();

    assert_eq!(buffer.capacity(), 0);
    assert_eq!(pools.stats().allocated_bytes, 0);
}

#[kithara::test]
fn shrinking_samples_releases_removed_capacity_charge() {
    const CAPACITY: usize = 16;
    const RETAINED: usize = CAPACITY / 2;

    let pools = TestPools::region(
        OverallBudget(CAPACITY * size_of::<f32>()),
        config(32),
        config(8),
    )
    .unwrap_or_else(|error| panic!("test region: {error}"));
    let mut samples = pools
        .get_with_len::<f32>(CAPACITY)
        .unwrap_or_else(|error| panic!("sample buffer: {error}"));
    let before = pools.stats().allocated_bytes;
    samples.drain(..CAPACITY - RETAINED);

    samples.shrink_to_fit();

    assert!(samples.capacity() >= RETAINED);
    assert!(pools.stats().allocated_bytes < before);
    assert_eq!(
        pools.stats().allocated_bytes,
        samples.capacity() * size_of::<f32>()
    );
}

mod generic_keys {
    use super::*;

    pub(super) enum NumbersTag {}
    pub(super) enum TextTag {}
    pub(super) type Numbers = PoolAlias<NumbersTag, VecKey<u64, 1>>;
    pub(super) type Text = PoolAlias<TextTag, StringKey<1>>;

    pool_schema! {
        pub(super) GenericPools {
            numbers: Numbers,
            text: Text,
        }
    }

    pub(super) fn pools(
        max_bytes: usize,
        max_retained_capacity: usize,
    ) -> PoolRegion<GenericPools> {
        let config = || {
            PoolConfig::builder()
                .max_buffers(1)
                .max_retained_capacity(max_retained_capacity)
                .build()
        };
        GenericPools::builder(OverallBudget(max_bytes))
            .numbers(config())
            .text(config())
            .build()
            .unwrap_or_else(|error| panic!("generic region: {error}"))
    }
}

#[kithara::test]
fn generic_vector_and_string_keys_share_the_region_budget() {
    let pools = generic_keys::pools(16, 0);
    let mut numbers = pools.get::<generic_keys::Numbers>();
    numbers
        .try_extend([1, 2])
        .unwrap_or_else(|error| panic!("numbers: {error}"));
    let mut text = pools.get::<generic_keys::Text>();

    assert!(matches!(
        text.try_push_str("x"),
        Err(PoolError::OverallBudgetExceeded { .. })
    ));
    assert_eq!(&*numbers, &[1, 2]);
    assert!(text.is_empty());
    assert_eq!(pools.stats().allocated_bytes, 16);
}

#[kithara::test]
fn generic_keys_report_reuse_and_rejected_returns() {
    let pools = generic_keys::pools(64, 1);
    let mut numbers = pools.get::<generic_keys::Numbers>();
    numbers
        .try_extend([1, 2])
        .unwrap_or_else(|error| panic!("numbers: {error}"));
    drop(numbers);

    let stats = pools.pool_stats::<generic_keys::Numbers>();
    assert_eq!(stats.alloc_misses, 1);
    assert_eq!(stats.put_drops, 1);
}

#[kithara::test]
fn idle_capacity_on_other_shards_does_not_refuse_a_live_request() {
    const MIB: usize = 1024 * 1024;
    let pools = pools(4 * MIB);

    for turn in 0..64 {
        let outcome = thread::scope(|scope| {
            scope
                .spawn(|| pools.get_with_len::<u8>(MIB).map(drop))
                .join()
                .unwrap_or_else(|_| panic!("turn {turn} panicked"))
        });
        assert!(
            outcome.is_ok(),
            "turn {turn} was refused while every earlier buffer had been dropped: {outcome:?}"
        );
    }
    assert!(pools.stats().allocated_bytes <= 4 * MIB);
}

#[kithara::test]
fn sample_reservation_grows_capacity_without_writing_samples() {
    let pools = pools(4096);
    let mut samples = pools.get::<f32>();

    samples
        .try_reserve(64)
        .unwrap_or_else(|error| panic!("reserve samples: {error}"));

    assert!(samples.is_empty());
    assert!(samples.capacity() >= 64);
}

#[kithara::test]
fn sample_reservation_stays_under_the_region_budget() {
    let pools = pools(64);
    let mut samples = pools.get::<f32>();

    assert!(samples.try_reserve(64).is_err());
    assert!(samples.is_empty());
}

#[kithara::test]
fn sample_reservation_is_additional_and_preserves_existing_samples_on_refusal() {
    let pools = pools(64);
    let mut samples = pools.get::<f32>();
    samples
        .try_extend_from_slice(&[0.25, -0.5])
        .expect("initial samples");
    samples.try_reserve(3).expect("room for five samples");
    assert!(samples.capacity() >= 5);
    assert_eq!(&*samples, &[0.25, -0.5]);

    assert!(matches!(
        samples.try_reserve(64),
        Err(PoolError::OverallBudgetExceeded { .. })
    ));
    assert_eq!(&*samples, &[0.25, -0.5]);
    assert!(matches!(
        samples.try_reserve(usize::MAX),
        Err(PoolError::CapacityOverflow { .. })
    ));
    assert_eq!(&*samples, &[0.25, -0.5]);
}

#[kithara::test]
fn sample_capacity_shrinks_without_initializing_or_discarding_samples() {
    let pools = pools(4096);
    let mut samples = pools.get::<f32>();
    samples
        .try_reserve(64)
        .expect("empty scratch capacity fits");
    let before = pools.stats().allocated_bytes;

    samples.shrink_to(16);
    assert!(samples.is_empty());
    assert!(samples.capacity() >= 16);
    assert!(pools.stats().allocated_bytes < before);
    assert_eq!(
        pools.stats().allocated_bytes,
        samples.capacity() * size_of::<f32>()
    );

    samples
        .try_extend_from_slice(&[0.25, -0.5])
        .expect("existing scratch holds two samples");
    samples.shrink_to(0);
    assert_eq!(&*samples, &[0.25, -0.5]);
    assert_eq!(
        pools.stats().allocated_bytes,
        samples.capacity() * size_of::<f32>()
    );
    let capacity = samples.capacity();
    samples.shrink_to(64);
    assert_eq!(samples.capacity(), capacity);
    assert_eq!(&*samples, &[0.25, -0.5]);
}
