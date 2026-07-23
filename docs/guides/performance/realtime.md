# Real-time callback & locking

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

## RT-audio callback

**Blocking primitive on the RT thread** (anchor - the whole section is one invariant)
The device callback runs under a hard sub-ms deadline; any lock, `.await`, or heap op can be preempted or wait unboundedly and produce an xrun. Cross the boundary with a wait-free SPSC ring for PCM/commands and atomics/`ArcSwap` for state the callback reads.
```rust
// bad: guard + async recv inside the device callback
fn process(&mut self, out: &mut [f32]) {
    let g = self.state.lock();       // parking_lot guard on RT thread
    let pcm = self.rx.recv().await;  // blocking/await recv on RT thread
}
// good: ring pop + lock-free snapshot reads
#[kithara::rtsan_forbid_blocking]
fn process(&mut self, out: &mut [f32]) {
    while let Some(frame) = self.cons.pop() { /* preallocated slots */ }
    if self.shared.playing.load(Relaxed) { /* atomic, no lock */ }
}
```
*tier: hot | detector: `#[kithara::rtsan_forbid_blocking]` + rtsan/no_block lanes (manual) | present in kithara*

**Logging / syscall / I/O on the callback** (I4 + I5)
No `tracing`/`println!`/file/socket on the RT path - the subscriber formats, allocates, and locks on the emitting thread. `#[kithara::probe]` (USDT) is the sanctioned RT-safe trace; otherwise write a relaxed counter the UI polls. A `warn!` is tolerable only on an already-lost cold branch (panic/evict).
```rust
// bad
fn process(&mut self, out: &mut [f32]) { info!(n = out.len(), "cb"); } // fmt+alloc+lock
// good
fn process(&mut self, out: &mut [f32]) { self.shared.xruns.fetch_add(1, Relaxed); }
```
*tier: hot | detector: rust.no-debug-prints (enforced ast-grep - `println!`/`dbg!` only; tracing is manual) | present in kithara*

**Unbounded work / alloc per callback** (I6)
Preallocate to max size at `prepare()`/`new_stream()` and reuse (`clear()` + refill, or bufpool). No `Vec::new`/`Box`/`format!`/HashMap-grow in the body.
```rust
// bad
fn process(&mut self, out: &mut [f32]) { let mut s = vec![0.0; out.len()]; /* heap */ }
// good
fn new_stream(&mut self, max: usize) { self.scratch.resize(max, 0.0); }
fn process(&mut self, out: &mut [f32]) { let s = &mut self.scratch[..out.len()]; }
```
*tier: hot | detector: rtsan + `assert_no_alloc` lane (manual) | present in kithara*

**std/tokio/crossbeam channel across the RT boundary** (I7)
Use a fixed-capacity `ringbuf` SPSC; the RT side only push/pop preallocated slots and coalesces/drops on full. mpsc allocs nodes and can park.
```rust
// bad
let (tx, rx) = tokio::sync::mpsc::channel(N);      // alloc + park
// good
let (prod, cons) = ringbuf::HeapRb::new(N).split(); // HeapProd / HeapCons
```
*tier: hot | detector: arch.no-raw-tokio-sync (enforced ast-grep) | already-enforced*

**Deallocation on the RT thread** (I8)
Never let the last `Arc`/`Vec`/`Box` drop inside `process()` - `free()` runs on the callback. Hand the released value back over a ring to an off-RT collector.
```rust
// bad
fn process(&mut self) { self.tracks.remove(i); }        // Drop -> free() on RT
// good
fn process(&mut self) { self.shared.discard_track(track); } // freed off-RT
```
*tier: hot | detector: rtsan + `assert_no_alloc` lane (manual) | present in kithara*

**Pool-miss fallback that hides a budget bug** (I9)
Pre-fill the pool to a computed budget at startup and count every miss so the budget can be fixed; keep the alloc-on-miss fallback off the rtsan produce core. A silent fallback masks a broken budget contract.
```rust
// bad: silent heap alloc on miss, no accounting
let buf = pool.get().unwrap_or_else(|| vec![0.0; n]);
// good: pooled reuse; stat_alloc_misses bumped on the fallback path
let buf = pool.get_with(|b| { b.clear(); b.resize(n, 0.0); });
```
*tier: hot | detector: perf.prefer-{byte,pcm}-pool (enforced ast-grep) + miss-count review (manual) | present in kithara*

**RT thread priority not promoted** (I2) - *watch:* N/A in kithara source: the device callback thread is created and QoS-promoted by cpal/firewheel, and the decode/produce threads are ring-decoupled from the callback deadline (degrade-on-underrun), not workgroup-bound. *tier: hot | detector: manual | preventive*

**Mean latency hides xruns** (I3) - *watch:* assert worst-case per-callback duration and make underrun counters first-class test output; never gate a DSP kernel on criterion mean/median. Pair with the rtsan/no_block lanes so blocking is caught even when timing passes. Belongs in the bench/test-harness doc. *tier: n/a | detector: manual | preventive*

**Sampling profiler perturbs the RT thread** (I10) - *watch:* for Apple-Silicon benches set explicit QoS on benched threads, reach thermal steady state (same machine/power), corroborate with instruction counts, and verify the shipping callback runs at RT priority separately. *tier: n/a | detector: manual | preventive*

## Sync & atomics

**Hot read-mostly state: ArcSwap, not `RwLock::read()`**
```rust
// bad: platform RwLock read on every audio block
let map = self.offsets.read();          // parking_lot RwLock
let byte = map.byte_for(frame);
// good: lock-free load; writers RCU via store(Arc::new(next))
let map = self.offsets.load();          // arc_swap::ArcSwap<ByteMap>
let byte = map.byte_for(frame);
```
`.load()` is wait-free; a `.read()` still CASes a shared cacheline under contention. Serialize concurrent writers with a small write-side `Mutex` only when load-mutate-store must not race. Corollary (no channel round-trip for a snapshot): readers poll `shared_state.snapshot()` (Relaxed atomic loads of position/duration/frontier); keep channels for commands/ownership transfer, not for asking the owner "where are we" each tick.
*tier: warm | detector: manual (candidate M3 ast-grep: `.read()` in a per-callback fn on a platform RwLock) | present in kithara (offsets.rs migrated RwLock -> ArcSwap)*

**Weakest ordering that carries the invariant, not reflexive SeqCst**
```rust
// bad: SeqCst by habit
stats.count.fetch_add(1, SeqCst);
// good: match ordering to the actual handoff
stats.count.fetch_add(1, Relaxed);      // independent counter/stat
ready.store(true, Release);             // publish data, then flag...
if ready.load(Acquire) { use(&data); }  // ...paired Acquire consumes it
```
SeqCst is a full barrier on ARM; an independent counter needs none. Reserve SeqCst for genuine total-order protocols (kithara keeps it on seek/play control flags) and document why. Downgrading an existing SeqCst is bench-gated and correctness-sensitive, never a blanket rewrite.
*tier: hot | detector: manual | present in kithara (play/seek flags SeqCst; stats Relaxed; seqlock AcqRel/Release+fence)*

**One sanctioned lock, kept off RT-adjacent paths**
```rust
// bad: raw parking_lot / std::sync sprinkled per crate
use parking_lot::Mutex;
// good: the platform wrapper (parking_lot native, wasm impl, poison+hang centralized)
use kithara_platform::sync::Mutex;
```
parking_lot has no priority inheritance on Apple, so keep it off any lock an RT/high-QoS thread can contend; cross the RT boundary with atomics/SPSC instead (see RT-callback discipline). The std<->parking_lot "swap for speed" debate is already settled workspace-wide.
*tier: hot | detector: enforced ast-grep `arch.no-std-sync-mutex` + devtools `platform_layer_hygiene` | already-enforced*

**No priority inversion: high-QoS thread must not wait on a lower one**
```rust
// bad: RT/produce thread parks on a lock a low-prio task holds
let g = shared.lock(); feed(&g);
// good: RT pulls a pre-filled ring, degrades on underrun
match cons.pop() { Some(frames) => play(frames), None => emit_silence() }
```
Without priority inheritance the RT thread stalls behind a preempted producer. Where waiting is legitimate, use an ownership-carrying platform `Mutex` and set explicit QoS on the producer thread.
*tier: warm | detector: manual | present in kithara (ring-decoupled produce; `block_on_underrun` is offline-opt-in only)*

**No thundering-herd wakeups**
```rust
// bad: one global Semaphore fronting all downloads
GLOBAL_DL.acquire().await;
// good: shard per loader lane; hand each item to exactly one receiver
lane.permits.acquire().await;           // per-track lane
// mpsc = one worker per unit of work; watch = latest-value fan-out
```
`notify_waiters`/broadcast/condition-poll wake N threads to fight over one slot; `notify_one`/`Semaphore` wake bounded waiters, and per-lane sharding avoids the herd entirely.
*tier: warm | detector: manual | present in kithara (loader lanes isolated per-track)*

**Watch-for (absent in kithara; no example needed)**
- **Non-lock-free `AtomicCell<T>`**: crossbeam `AtomicCell` on an oversized `T` silently degrades to an internal spinlock; if ever introduced, static-assert `AtomicCell::<T>::is_lock_free()` or pack into a `u64`. None present (small state via plain atomics/ArcSwap). *tier: hot | detector: none | preventive*
- **Naive spin-lock on the RT thread**: hand-rolled `loop { try_lock() }` or the `spin` crate busy-waiting on the audio thread. None present; RT sharing is lock-free ring/atomics/seqlock, non-RT goes through the platform `Mutex`. Existing `spin_loop()` is a bounded seqlock retry, not a busy-wait lock. *tier: hot | detector: none | preventive*
- **Naive (non-atomic-payload) seqlock**: reading the payload with plain non-atomic loads is UB even on discarded retries; read per-word atomics (Relaxed) + `fence(Acquire)` before the version recheck. kithara's `SeqAnchorCell` (`kithara-hls/src/variant/flow/seqlock.rs`) is the correct reference form. *tier: hot | detector: none | already-correct*
- **Single global hot atomic counter**: a many-thread `fetch_add` on one cacheline; shard with `CachePadded` or accumulate per-block and flush once with a Relaxed add. kithara's static atomics are rare ID generators (one bump per worker/bus), not contended. *tier: warm | detector: manual | preventive*
