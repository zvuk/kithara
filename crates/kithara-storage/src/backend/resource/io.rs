#![forbid(unsafe_code)]

use kithara_test_utils::kithara;

use crate::{
    StorageError, StorageResult,
    backend::{resource::state::ResourceCore, traits::DriverIo},
};

impl<D: DriverIo> ResourceCore<D> {
    /// Takes a lock-free fast path once the resource is committed: a committed resource exposes an
    /// immutable snapshot, so reads go straight to it with no state mutex.
    #[kithara::measure]
    pub(super) fn read_at_inner(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if let Some(committed_len) = self.inner.driver.committed_len() {
            if self.inner.cancel.is_cancelled() {
                return Err(StorageError::Cancelled);
            }
            if offset >= committed_len {
                return Ok(0);
            }
            let available = usize::try_from(committed_len - offset).unwrap_or(usize::MAX);
            let to_read = buf.len().min(available);
            if let Some(read) = self
                .inner
                .driver
                .read_committed(offset, &mut buf[..to_read])?
            {
                return Ok(read);
            }
        }

        self.check_health()?;

        let effective_len = {
            let final_len = self.inner.gate.lock().final_len;
            let storage_len = self.inner.driver.storage_len();
            let data_len = final_len.unwrap_or(storage_len);
            data_len.min(storage_len)
        };

        if offset >= effective_len {
            return Ok(0);
        }

        let available = usize::try_from(effective_len - offset).unwrap_or(usize::MAX);
        let to_read = buf.len().min(available);

        self.inner
            .driver
            .read_at(offset, &mut buf[..to_read], effective_len)
    }

    /// Read the **active working storage**, bypassing the lock-free committed
    /// snapshot fast path used by [`read_at_inner`](Self::read_at_inner).
    ///
    /// A re-download keeps the prior generation's snapshot published for
    /// concurrent readers while it rewrites the working storage. The writer's
    /// own read-modify-write (decrypt on commit) must observe the *in-flight*
    /// bytes it just wrote, not the stale snapshot — so it reads through here.
    /// `driver.read_at` is working-buffer-first, so this returns the in-flight
    /// generation while it is being written and the committed bytes otherwise.
    pub(super) fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.check_health()?;

        let effective_len = {
            let final_len = self.inner.gate.lock().final_len;
            let storage_len = self.inner.driver.storage_len();
            final_len.unwrap_or(storage_len).min(storage_len)
        };

        if offset >= effective_len {
            return Ok(0);
        }

        let available = usize::try_from(effective_len - offset).unwrap_or(usize::MAX);
        let to_read = buf.len().min(available);

        self.inner
            .driver
            .read_at(offset, &mut buf[..to_read], effective_len)
    }

    /// Every write publishes its availability and retires the snapshot it displaced,
    /// so the frees stay on the write side.
    #[kithara::measure]
    pub(super) fn write_at_inner(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        if data.is_empty() {
            return Ok(());
        }

        self.check_health()?;

        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(StorageError::InvalidRange {
                start: offset,
                end: u64::MAX,
            })?;

        let committed = {
            let state = self.inner.gate.lock();
            state.committed
        };

        self.inner.driver.write_at(offset, data, committed)?;

        let range = offset..end;

        {
            let state = self.inner.gate.lock();
            let upper = state.final_len.unwrap_or(u64::MAX);
            self.inner.edit_available(state, |available| {
                available.insert(range.clone());
                if let Some(window) = self.inner.driver.valid_window() {
                    if window.start > 0 {
                        available.remove(0..window.start);
                    }
                    if window.end < upper {
                        available.remove(window.end..upper);
                    }
                }
            });
        }
        self.inner.gate.notify_all();

        if let Some(observer) = self.inner.observer.as_ref() {
            observer.on_write(range);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{
        ops::Range,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
    };

    use kithara_platform::{
        CancelToken,
        sync::{Arc, Mutex, Weak},
        thread,
        time::Duration,
    };
    use rangemap::RangeSet;

    use crate::{
        ResourceStatus, WaitOutcome,
        backend::{
            memory::driver::{MemDriver, MemOptions},
            mmap::driver::{MmapDriver, MmapOptions},
            resource::state::ResourceCore,
            traits::{AvailabilityObserver, DriverIo},
        },
        test_pools::{byte_buffer, pools},
    };

    /// Open a fresh, uncommitted in-memory resource for the concurrency tests.
    fn open_mem() -> ResourceCore<MemDriver> {
        let pools = pools();
        ResourceCore::open(
            CancelToken::never(),
            MemOptions::builder().buffer(byte_buffer(&pools)).build(),
        )
        .expect("open mem must succeed")
    }

    /// Open a fresh, uncommitted mmap-backed resource at `path` (mode/len at
    /// builder defaults). The caller keeps the backing `TempDir` alive for the
    /// duration of the test.
    fn open_mmap(path: std::path::PathBuf) -> ResourceCore<MmapDriver> {
        ResourceCore::open(CancelToken::never(), MmapOptions::for_path(path).build())
            .expect("open mmap must succeed")
    }

    /// Backend selector for the `#[case]`-parameterized concurrency tests. The
    /// two backends differ by *type* (`ResourceCore<MemDriver>` vs
    /// `ResourceCore<MmapDriver>`), so each case dispatches to the generic test
    /// body with the matching driver.
    #[derive(Debug, Clone, Copy)]
    enum Backend {
        Mem,
        Mmap,
    }

    /// Reads back the status of the resource it observes at the moment that
    /// resource records its commit.
    #[derive(Default)]
    struct CommitWitness {
        core: Mutex<Option<ResourceCore<MemDriver>>>,
        seen: Mutex<Option<ResourceStatus>>,
    }

    impl AvailabilityObserver for CommitWitness {
        fn on_commit(&self, _final_len: u64) {
            let status = self.core.lock().as_ref().map(ResourceCore::status_inner);
            *self.seen.lock() = status;
        }

        fn on_write(&self, _range: Range<u64>) {}
    }

    /// The commit record must land before the resource reports `Committed`:
    /// a byte-capped cache that sees `Committed` may evict the resource and drop
    /// its record, and a record landing after that revives the availability of a
    /// resource nobody can open any more.
    #[kithara::test]
    fn a_commit_is_recorded_before_it_is_published() {
        let pools = pools();
        let witness = Arc::new(CommitWitness::default());
        let core: ResourceCore<MemDriver> = ResourceCore::open_with_observer(
            CancelToken::never(),
            MemOptions::builder().buffer(byte_buffer(&pools)).build(),
            Some(Arc::clone(&witness) as Arc<dyn AvailabilityObserver>),
        )
        .expect("open mem must succeed");
        *witness.core.lock() = Some(core.clone());
        core.write_at_inner(0, b"hello world")
            .expect("active write must succeed");

        core.commit_inner(Some(11)).expect("commit must succeed");
        let seen = witness.seen.lock().take();
        witness.core.lock().take();

        assert!(
            matches!(seen, Some(ResourceStatus::Active)),
            "the commit was already visible when it was recorded: {seen:?}"
        );
        assert!(matches!(
            core.status_inner(),
            ResourceStatus::Committed {
                final_len: Some(11)
            }
        ));
    }

    /// A committed resource's `read_at_inner` must complete WITHOUT taking the
    /// `inner.gate` state lock. The test holds that lock on this thread while a
    /// worker thread reads; a lock-free fast path completes immediately, a slow
    /// path blocks on the held guard and times out.
    #[kithara::test]
    fn committed_read_does_not_take_state_mutex() {
        let pools = pools();
        let core: ResourceCore<MemDriver> = ResourceCore::open(
            CancelToken::never(),
            MemOptions::builder()
                .buffer(byte_buffer(&pools))
                .initial_data(b"hello world".to_vec())
                .build(),
        )
        .expect("BUG: MemDriver::open with initial_data is infallible");

        assert_eq!(core.inner.driver.committed_len(), Some(11));

        let guard = core.inner.gate.lock();

        let (tx, rx) = mpsc::channel();
        let worker = core.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 11];
            let result = worker.read_at_inner(0, &mut buf);
            tx.send(result.map(|n| (n, buf))).ok();
        });

        let received = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("committed read blocked on the state mutex");

        drop(guard);

        let (n, buf) = received.expect("committed read failed");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    /// A committed resource's `len_inner`/`contains_range_inner` must answer
    /// WITHOUT taking the `inner.gate` state lock. Mirrors the read fast-path test:
    /// that lock is held on this thread while a worker queries length and
    /// coverage; a lock-free path completes immediately, a locking path blocks
    /// on the held guard and times out.
    #[kithara::test]
    fn committed_len_and_contains_do_not_take_state_mutex() {
        let pools = pools();
        let core: ResourceCore<MemDriver> = ResourceCore::open(
            CancelToken::never(),
            MemOptions::builder()
                .buffer(byte_buffer(&pools))
                .initial_data(b"hello world".to_vec())
                .build(),
        )
        .expect("BUG: MemDriver::open with initial_data is infallible");

        assert_eq!(core.inner.driver.committed_len(), Some(11));

        let guard = core.inner.gate.lock();

        let (tx, rx) = mpsc::channel();
        let worker = core.clone();
        thread::spawn(move || {
            let len = worker.len_inner();
            let covers_exact = worker.contains_range_inner(0..11);
            let covers_over = worker.contains_range_inner(0..12);
            tx.send((len, covers_exact, covers_over)).ok();
        });

        let (len, covers_exact, covers_over) = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("committed len/contains blocked on the state mutex");

        drop(guard);

        assert_eq!(len, Some(11));
        assert!(covers_exact);
        assert!(!covers_over);

        assert_eq!(core.len_inner(), Some(11));
        assert!(core.contains_range_inner(0..11));
        assert!(!core.contains_range_inner(0..12));
    }

    /// An active resource's `contains_range_inner` must answer from a lock-free
    /// availability snapshot. The committed-length fast path is intentionally
    /// unavailable here: the resource is still active after `write_at_inner`.
    fn run_active_contains_range_does_not_take_state_mutex<D>(core: &ResourceCore<D>)
    where
        D: DriverIo + Send + Sync + 'static,
    {
        core.write_at_inner(0, b"hello world")
            .expect("active write must succeed");
        assert_eq!(core.inner.driver.committed_len(), None);

        let guard = core.inner.gate.lock();

        let (tx, rx) = mpsc::channel();
        let worker = core.clone();
        thread::spawn(move || {
            let covers = worker.contains_range_inner(0..11);
            tx.send(covers).ok();
        });

        let covers = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("active contains_range blocked on the state mutex");

        drop(guard);

        assert!(covers);
    }

    #[kithara::test]
    #[case::mem(Backend::Mem)]
    #[case::mmap(Backend::Mmap)]
    fn active_contains_range_does_not_take_state_mutex(#[case] backend: Backend) {
        match backend {
            Backend::Mem => run_active_contains_range_does_not_take_state_mutex(&open_mem()),
            Backend::Mmap => {
                let dir = tempfile::tempdir().expect("tempdir must be creatable");
                run_active_contains_range_does_not_take_state_mutex(&open_mmap(
                    dir.path().join("active.bin"),
                ));
            }
        }
    }

    /// A read never takes ownership of the snapshot, however long the write
    /// side stays quiet: the produce path reads at audio-tick cadence (~94
    /// ticks/s at 48 kHz with 512-frame blocks) while writes arrive at
    /// download cadence. The resource stays active, so every read of the
    /// ten-second burst answers from the snapshot.
    #[kithara::test]
    fn a_read_burst_never_leaks_a_generation() {
        const TICKS_PER_SECOND: usize = 94;
        const BURST: usize = TICKS_PER_SECOND * 10;

        let core = open_mem();
        core.write_at_inner(0, b"hello world")
            .expect("active write must succeed");
        let snapshot = core.inner.available.load_full();
        let owners = Arc::strong_count(&snapshot);

        let covered = (0..BURST)
            .filter(|_| core.contains_range_inner(0..11))
            .count();

        assert_eq!(
            covered, BURST,
            "every read in the burst answers from the snapshot"
        );
        assert_eq!(
            Arc::strong_count(&snapshot),
            owners,
            "{BURST} reads with no intervening write took ownership of the snapshot"
        );
    }

    /// A reader overlapping writes is never the last owner of a snapshot they
    /// displaced: its guard drop leaves the snapshot to the writer, and the
    /// first write after that drop frees it.
    #[kithara::test]
    fn displaced_snapshot_is_freed_by_the_writer() {
        let core = open_mem();
        core.write_at_inner(0, b"hello")
            .expect("active write must succeed");
        let reader = core.inner.available.load();
        let displaced: Weak<RangeSet<u64>> = Arc::downgrade(&reader);

        core.write_at_inner(5, b" world")
            .expect("active write must succeed");
        core.write_at_inner(11, b"!")
            .expect("active write must succeed");
        drop(reader);
        assert!(
            displaced.upgrade().is_some(),
            "a reader guard drop never frees the displaced snapshot"
        );

        core.write_at_inner(12, b"?")
            .expect("active write must succeed");
        assert!(
            displaced.upgrade().is_none(),
            "the next write frees the quiesced snapshot"
        );
    }

    /// A write frees what it retires only after releasing the gate, so a
    /// produce-core read that takes the gate never waits on those frees. The
    /// test holds the retire list: the write publishes and then parks on it,
    /// and a gated read of the still active resource must answer meanwhile.
    #[kithara::test]
    fn a_gated_read_never_waits_on_retired_frees() {
        let core = open_mem();
        let retired = core.inner.retired.lock();

        let writer = core.clone();
        let (written_tx, written_rx) = mpsc::channel();
        thread::spawn(move || {
            written_tx.send(writer.write_at_inner(0, b"hello")).ok();
        });
        while !core.contains_range_inner(0..5) {
            thread::yield_now();
        }

        let reader = core.clone();
        let (len_tx, len_rx) = mpsc::channel();
        thread::spawn(move || {
            len_tx.send(reader.len_inner()).ok();
        });
        len_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("a gated read waited on the frees of a write");
        drop(retired);

        written_rx
            .recv()
            .expect("the writer reports its write")
            .expect("active write must succeed");
    }

    /// Fill `[0, len)` with `value` in multiple `write_at_inner` calls so a
    /// reader interleaving between chunks can observe a half-rewritten buffer.
    fn write_generation<D: DriverIo>(core: &ResourceCore<D>, value: u8, len: usize, chunk: usize) {
        let mut off = 0usize;
        while off < len {
            let end = (off + chunk).min(len);
            let data = vec![value; end - off];
            core.write_at_inner(off as u64, &data)
                .expect("active chunk write must succeed");
            off = end;
        }
    }

    /// Concurrency contract: while a writer cycles a resource through
    /// `reactivate → multi-chunk rewrite → commit`, stamping a **distinct
    /// byte value per generation**, a concurrent reader that gates on
    /// `contains_range` (the real reader contract: confirm availability,
    /// then read) must NEVER observe a *torn* read — a single read whose
    /// bytes mix two generations.
    ///
    /// A torn read means the read path admitted bytes past the confirmed,
    /// fully-written prefix (e.g. the slow path bounds by `storage_len`
    /// instead of the committed/available prefix, or `reactivate` left
    /// `available` reporting a range that is mid-rewrite). This is the
    /// pure storage-layer manifestation of the data race that surfaces, one
    /// layer up, as a decoder "producer failed" on the ephemeral DRM path.
    /// Deterministic under stress: every generation opens a fresh tear
    /// window and the readers hammer it. Parameterized over both backends
    /// (`_mem` / `_mmap`) so the contract is proven for the in-memory
    /// (ephemeral) snapshot path AND the file-backed mmap path.
    fn run_concurrent_generation_rewrite<D>(core: &ResourceCore<D>)
    where
        D: DriverIo + Send + Sync + 'static,
    {
        const LEN: usize = 8192;
        const CHUNK: usize = 256;
        const GENS: u8 = 250;
        const READERS: usize = 6;

        write_generation(core, 1, LEN, CHUNK);
        core.commit_inner(Some(LEN as u64))
            .expect("baseline commit must succeed");

        let stop = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..READERS)
            .map(|_| {
                let reader = core.clone();
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut buf = vec![0u8; LEN];
                    while !stop.load(Ordering::Relaxed) {
                        if !reader.contains_range_inner(0..LEN as u64) {
                            continue;
                        }
                        let Ok(n) = reader.read_at_inner(0, &mut buf) else {
                            continue;
                        };
                        if n == 0 {
                            continue;
                        }
                        let head = buf[0];
                        if let Some(pos) = buf[..n].iter().position(|&b| b != head) {
                            stop.store(true, Ordering::Relaxed);
                            panic!(
                                "TORN READ: byte[0]={head} but byte[{pos}]={} — \
                                 a single read mixed two generations (read {n} bytes)",
                                buf[pos]
                            );
                        }
                    }
                })
            })
            .collect();

        for value in 2..=GENS {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            core.reactivate_inner().expect("reactivate must succeed");
            write_generation(core, value, LEN, CHUNK);
            core.commit_inner(Some(LEN as u64))
                .expect("commit must succeed");
        }
        stop.store(true, Ordering::Relaxed);

        for reader in readers {
            reader.join().expect("reader detected a torn read");
        }
    }

    #[kithara::test]
    #[case::mem(Backend::Mem)]
    #[case::mmap(Backend::Mmap)]
    fn concurrent_generation_rewrite_never_tears_a_read(#[case] backend: Backend) {
        match backend {
            Backend::Mem => run_concurrent_generation_rewrite(&open_mem()),
            Backend::Mmap => {
                let dir = tempfile::tempdir().expect("tempdir must be creatable");
                run_concurrent_generation_rewrite(&open_mmap(dir.path().join("gen.bin")));
            }
        }
    }

    /// Same race, exercised through the real reader path the decode producer
    /// uses (`wait_range` → `read_at`) and with a **varying length per
    /// generation** — mirroring the DRM estimate→real (PKCS7 decrypt-shrink)
    /// and variant-switch shape where a re-download commits a different length.
    /// A `wait_range` that reports `Ready` must be followed by a `read_at` that
    /// returns bytes from a single generation (no torn read across the
    /// reactivate→rewrite→commit boundary). If this tears, the residual flake is
    /// still in storage; if it stays green, the storage layer is consistent and
    /// the residual lives one layer up (decoder recreation).
    fn run_concurrent_varying_length_rewrite<D>(core: &ResourceCore<D>)
    where
        D: DriverIo + Send + Sync + 'static,
    {
        const LONG: usize = 8192;
        const SHORT: usize = 2731;
        const CHUNK: usize = 256;
        const GENS: u8 = 250;
        const READERS: usize = 6;

        write_generation(core, 1, LONG, CHUNK);
        core.commit_inner(Some(LONG as u64))
            .expect("baseline commit must succeed");

        let stop = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..READERS)
            .map(|_| {
                let reader = core.clone();
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut buf = vec![0u8; LONG];
                    while !stop.load(Ordering::Relaxed) {
                        let Some(target) = reader.len_inner() else {
                            continue;
                        };
                        let target = usize::try_from(target).expect("test lengths fit usize");
                        if target == 0 {
                            continue;
                        }
                        match reader.wait_range_inner(0..target as u64, None) {
                            Ok(WaitOutcome::Ready) => {}
                            _ => continue,
                        }
                        let Ok(n) = reader.read_at_inner(0, &mut buf[..target]) else {
                            continue;
                        };
                        if n == 0 {
                            continue;
                        }
                        let head = buf[0];
                        if let Some(pos) = buf[..n].iter().position(|&b| b != head) {
                            stop.store(true, Ordering::Relaxed);
                            panic!(
                                "TORN READ (wait_range path): byte[0]={head} but byte[{pos}]={} — \
                                 a single read mixed two generations (read {n} of target {target})",
                                buf[pos]
                            );
                        }
                    }
                })
            })
            .collect();

        for value in 2..=GENS {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let len = if value % 2 == 0 { LONG } else { SHORT };
            core.reactivate_inner().expect("reactivate must succeed");
            write_generation(core, value, len, CHUNK);
            core.commit_inner(Some(len as u64))
                .expect("commit must succeed");
        }
        stop.store(true, Ordering::Relaxed);

        for reader in readers {
            reader.join().expect("reader detected a torn read");
        }
    }

    #[kithara::test]
    #[case::mem(Backend::Mem)]
    #[case::mmap(Backend::Mmap)]
    fn concurrent_varying_length_rewrite_via_wait_range_stays_consistent(#[case] backend: Backend) {
        match backend {
            Backend::Mem => run_concurrent_varying_length_rewrite(&open_mem()),
            Backend::Mmap => {
                let dir = tempfile::tempdir().expect("tempdir must be creatable");
                run_concurrent_varying_length_rewrite(&open_mmap(dir.path().join("gen.bin")));
            }
        }
    }
}
