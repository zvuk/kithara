# Performance

Hot-path antipatterns and their fixes, grouped by theme. Each entry is bad -> good with the detector that catches it (enforced ast-grep/clippy lint, the opt-in `cargo xtask audit-clippy` sweep, or manual). Crate tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.

## Allocation & memory

**Collect-then-iterate roundtrip**
```rust
// bad
let lens: Vec<_> = segs.iter().map(|s| s.len).collect();
let total: usize = lens.iter().sum();
// good
let total: usize = segs.iter().map(|s| s.len).sum();
```
*tier: warm | detector: `perf.no-collect-iter-roundtrip` (ast-grep) | already-enforced*

**Needless clone / clone-assign**
```rust
// bad
sink.push(item.clone());   // item only borrowed downstream
dst = src.clone();         // in a reuse loop: reallocates every pass
// good
sink.push(&item);          // or move once if truly consumed
dst.clone_from(&src);      // reuses dst's existing allocation
```
*tier: warm | detector: opt-in `cargo xtask audit-clippy` (redundant_clone / assigning_clones) | preventive*

**Grow without reserve**
```rust
// bad
let mut out = Vec::new();
for f in frames { out.push(map(f)); }
// good
let mut out = Vec::with_capacity(frames.len());  // beat analyzer: with_capacity(frames*2)
out.extend(frames.iter().map(map));
```
*tier: warm | detector: manual (red-flags "allocate in loops") | preventive*

**Hot-loop alloc / capacity churn**
```rust
// bad
for block in blocks {
    let mut scratch = vec![0.0; n];  // fresh alloc every block
    dsp(block, &mut scratch);
    scratch.shrink_to_fit();         // hands the pages right back
}
// good
let mut scratch = pcm_pool.get_with(|b| b.resize(n, 0.0));
for block in blocks { scratch.clear(); scratch.resize(n, 0.0); dsp(block, &mut scratch); }
```
`clear()` keeps capacity for the next pass; `shrink` only at lifecycle boundaries (track unload / cache evict) to a watermark, as bufpool's `shrink_to(trim)` does.
*tier: hot | detector: none (manual; overlaps `perf.prefer-*-pool`) | preventive*

**PCM/byte buffers outside kithara-bufpool** (flagship)
```rust
// bad
let mut buf = vec![0.0f32; frames * channels];  // RT/decode path
let raw = pcm_pool();                            // global accessor
// good
let mut buf = self.pcm_pool.get_with(|b| { b.clear(); b.resize(frames * channels, 0.0); });
// PooledOwned recycles on drop; take PcmPool/BytePool from the app top, never the global accessor
```
*tier: hot | detector: `perf.prefer-pcm-pool` / `perf.prefer-byte-pool` / `perf.no-global-pool-accessor` (ast-grep) | already-enforced*

**format! for concat / owned compare**
```rust
// bad
let path = format!("{}{}", dir, name);
if codec.to_string() == "flac" { .. }
// good
let mut path = dir.to_owned(); path.push_str(name);  // or write! / concat
if codec == "flac" { .. }
```
*tier: cold | detector: clippy `useless_format` / `cmp_owned` | already-enforced*

**Needless owning conversion**
```rust
// bad
fn open(url: String, key: Vec<u8>) {..}  // body only reads
// good
fn open(url: &str, key: &[u8]) {..}
```
*tier: warm | detector: clippy `unnecessary_to_owned` (+ `ptr_arg`=deny) | already-enforced*

**Double allocation**
```rust
// bad
cover: Arc<String>,  key: Arc<Vec<u8>>,
// good
cover: Arc<str>,     key: Arc<[u8]>,
```
*tier: warm | detector: clippy `redundant_allocation` / `box_collection` | already-enforced*

**vec! for fixed data**
```rust
// bad
for r in vec![Rate::Hz44100, Rate::Hz48000] { .. }
// good
for r in [Rate::Hz44100, Rate::Hz48000] { .. }
```
*tier: cold | detector: clippy `useless_vec` | already-enforced*

**drain-collect**
```rust
// bad
let taken: Vec<_> = buf.drain(..).collect();
// good
let taken = mem::take(&mut buf);  // whole buffer; else drain(..).extend/for_each keeps the tail
```
*tier: warm | detector: clippy `drain_collect` | already-enforced*

**Large value / fat enum variant**
```rust
// bad
enum Event { Tick, Decoded(PcmChunk /* inline, bloats every Event */) }
fn feed(chunk: PcmChunk) {..}
// good
enum Event { Tick, Decoded(Box<PcmChunk>) }
fn feed(chunk: &PcmChunk) {..}
```
An unboxed fat variant sizes *every* enum value to the largest arm.
*tier: hot | detector: clippy `large_enum_variant` / `needless_pass_by_value` | already-enforced*

**Write-only collection**
```rust
// bad
let mut seen = Vec::new();
for e in events { seen.push(e.id); }  // seen never read
// good
// delete seen and the loop that fills it
```
*tier: warm | detector: opt-in `cargo xtask audit-clippy` (collection_is_never_read) | preventive*

**SmallVec/array micro-swap** - watch for: swap `Vec` for `SmallVec`/array only after a benchmark proves the collection is genuinely usually-small; it is frequently cargo-culted and copies on spill. *tier: n/a | detector: manual | preventive*

## Ownership & API signatures

**Reference param over a smart-pointer type.** `ptr_arg=deny` keeps this clean.
```rust
// bad
fn open(s: &String, b: &Vec<u8>, p: &PathBuf) { ... }
// good
fn open(s: &str, b: &[u8], p: &Path) { ... }
```
*tier: warm | detector: clippy `ptr_arg` (deny) | already-enforced*

**By-value param the body only reads.**
```rust
// bad
fn score(cfg: DecodeConfig) -> f32 { cfg.gain * 2.0 }
// good
fn score(cfg: &DecodeConfig) -> f32 { cfg.gain * 2.0 }   // take by value only to move/store/consume
```
*tier: warm | detector: clippy `needless_pass_by_value` (warn) | already-enforced*

**`&mut` that never mutates.**
```rust
// bad
fn depth(buf: &mut RingCons) -> usize { buf.len() }
// good
fn depth(buf: &RingCons) -> usize { buf.len() }
```
*tier: warm | detector: opt-in `cargo xtask audit-clippy` (`needless_pass_by_ref_mut`) | preventive*

**Allocate an owned return to satisfy the borrow checker.**
```rust
// bad
fn codec_name(&self) -> String { self.name.to_string() }
// good
fn codec_name(&self) -> &str { &self.name }          // Cow<'_, str> when sometimes-owned
```
Allocate only for genuinely new data; hand back a borrow otherwise.
*tier: warm | detector: manual (overlaps clippy `unnecessary_to_owned`) | preventive*

**`&T` for a small `Copy` type.**
```rust
// bad
fn seek(pos: &u64, idx: &SegmentIdx) { ... }
// good
fn seek(pos: u64, idx: SegmentIdx) { ... }
```
*tier: warm | detector: clippy `trivially_copy_pass_by_ref` (warn) | already-enforced*

## Iterators & collections

**Unfused adapters (`map().flatten()`, `filter().map()`).**
```rust
// bad
segs.iter().map(|s| s.key.parse().ok())        // -> Iterator<Option<T>>
    .filter(Option::is_some).map(Option::unwrap)
segs.iter().map(load_keys).flatten()
// good
segs.iter().filter_map(|s| s.key.parse().ok())
segs.iter().flat_map(load_keys)
```
*tier: warm | detector: clippy `manual_filter_map`/`map_flatten`/`flat_map_option` | already-enforced*

**Clone before you narrow.**
```rust
// bad
frames.iter().cloned().filter(|f| f.voiced).take(8)
// good
frames.iter().filter(|f| f.voiced).take(8).cloned()   // clone only the survivors
```
*tier: hot | detector: opt-in `cargo xtask audit-clippy` (`iter_overeager_cloned`) | preventive*

**`.map(|x| x.clone())` instead of `.cloned()`/`.copied()`.**
Watch for: `iter.map(|x| x.clone())` - `.copied()` for `Copy`, `.cloned()` otherwise.
*tier: hot | detector: ast-grep `perf.prefer-cloned` + clippy `cloned_instead_of_copied` (deny) | already-enforced*

**`iter().count()` where a length exists.**
Watch for: `slice.iter().count()` / `s.chars()...count()` when `.len()` is O(1) - use `slice.len()`.
*tier: warm | detector: ast-grep `perf.no-iter-count` | already-enforced*

**Stable `sort_by` on floats/primitives.** Stability is meaningless under `total_cmp`; the stable sort still pays for an O(n) scratch buffer.
```rust
// bad
onsets.sort_by(f32::total_cmp);
// good
onsets.sort_unstable_by(f32::total_cmp);
```
Live sites: `analysis/beat/grid.rs`, `analysis/beat/analyzer.rs`, `kithara-beat/src/postprocess.rs`.
*tier: hot | detector: proposed ast-grep `perf.no-stable-sort-total-cmp` (M3); today manual | present in kithara*

**Full sort for top-k / median.**
Watch for: `v.sort(); &v[..k]` - use `v.select_nth_unstable_by(k, cmp)`. Not currently present.
*tier: n/a | detector: manual | preventive*

**`for ... { out.push(x) }` over an iterator.**
```rust
// bad
let mut out = Vec::new();
for s in segs.iter() { out.push(s.idx); }
// good
let out: Vec<_> = segs.iter().map(|s| s.idx).collect();   // or out.extend(segs.iter().map(|s| s.idx))
```
*tier: warm | detector: clippy `needless_for_each` + AGENTS red-flags | already-enforced*

**Manual index loop with bounds checks over one slice.**
Watch for: `for i in 0..slice.len() { slice[i] }` - use `.iter().enumerate()` / `chunks_exact(N)`; already applied to the trimmer in W1.
*tier: hot | detector: clippy `needless_range_loop` (deny) | already-enforced*

## Data-structure choice

**Linear membership scan in a loop (O(n|m))**
```rust
// bad: retain re-scans a Vec for every key (kithara-assets flush/core.rs:155)
let alive: Vec<usize> = live_keys().collect();
self.non_durable.retain(|k| alive.contains(k));
// good: build the set once, membership is O(1)
let alive: HashSet<usize> = live_keys().collect();
self.non_durable.retain(|k| alive.contains(k));
```
Most kithara membership already uses `&HashSet` (assets LRU/pin); the flush retain is the live exception.
*tier: warm | detector: manual (backlog `.audit/rules/audit.linear-scan-in-loop.yml`) | present in kithara*

**contains_key + insert double lookup**
```rust
// bad: hashes and probes k twice
if !map.contains_key(&k) { map.insert(k, make()); }
// good: one lookup, value built only on vacancy
map.entry(k).or_insert_with(make);
```
*tier: warm | detector: clippy `map_entry` (default-on) | already-enforced (no occurrence)*

**HashMap + post-sort instead of BTreeMap**
```rust
// bad: hash map re-sorted every time you need ordered output
let mut out: Vec<_> = map.iter().collect();
out.sort_by_key(|(k, _)| *k);
// good: BTreeMap yields keys in order for free
for (k, v) in &ordered { /* ... */ }
```
Only switch when you *repeatedly* iterate in key order; a one-shot sort of a small Vec (abr candidates) is fine, don't over-rotate to BTreeMap.
*tier: warm | detector: manual | preventive (not present)*

**Vec::remove(0) / insert(0,..) as a FIFO (O(n) shift)**
```rust
// bad: remove(0) shifts every remaining element on each pop
let next = queue.remove(0);
// good: VecDeque, O(1) at both ends
let next = queue.pop_front();   // queue.push_back(item)
```
The hot gapless trimmer queue was moved off `remove(0)` in W1; remaining `remove(0)` lives only in cold sim waker lists (`kithara-platform/src/flash/tokio/mpsc.rs`).
*tier: hot | detector: manual (backlog ast-grep `audit.vec-as-queue`, promotable) | hot instance already fixed*

**ZST map values** - watch for `HashMap<K, ()>` used as a set; use `HashSet<K>`. Absent today (sets are declared `HashSet`); would surface via the opt-in pedantic sweep if introduced. *tier: n/a | detector: clippy `zero_sized_map_values` (pedantic, opt-in) | preventive*

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

## Async & concurrency

**Lock guard held across `.await`**
```rust
// bad: parking_lot guard alive at the await point -> blocks tasks, deadlock risk
let g = self.state.lock();
fetch(&g.url).await;
// good: clone out under the sync guard, drop it, then await
let url = { self.state.lock().url.clone() };
fetch(&url).await;
```
*tier: warm | detector: clippy `await_holding_lock` + devtools `await_under_guard` | already-enforced*

**Blocking / heavy CPU in an async context** (same concern as `L1`)
```rust
// bad: decode/CPU or a blocking syscall on a tokio worker stalls the reactor
async fn run(&self) { let pcm = decode_whole_file(&src); }
// good: long-lived decode on an owned RT thread; one-shot blocking via bounded spawn_blocking
let decoder = spawn_blocking(move || Decoder::open(src)).await?;
```
*tier: hot | detector: enforced ast-grep `arch.no-raw-no-block` + rtsan-async lane | already-enforced*

**Lock scope wider than the critical section**
```rust
// bad: guard held through unrelated work
let g = self.map.lock();
let v = g.get(&k).copied();
do_unrelated_work();          // still locked
// good: scope the guard to just the access
let v = { self.map.lock().get(&k).copied() };
do_unrelated_work();
```
*tier: warm | detector: clippy `significant_drop_tightening` (enabled) | preventive*

**Sequential await of independent futures**
```rust
// bad: independent fetches serialized
let key = load_key(u1).await?;
let seg = load_seg(u2).await?;
// good: run concurrently, short-circuit on first error (HLS playlist/key loads)
let (key, seg) = try_join!(load_key(u1), load_seg(u2))?;   // or try_join_all(futs)
```
*tier: warm | detector: ast-grep `idioms.join-of-join-alls` (narrow) / manual | already-enforced*

**Write-lock where a read-lock suffices**
```rust
// bad
let n = state.write().len();
// good
let n = state.read().len();
```
*tier: warm | detector: clippy `readonly_write_lock` (default warn) | preventive*

**`Mutex<bool>`/`Mutex<int>` for a lone flag**
```rust
// bad: control flag behind a mutex
inserted: Mutex<bool>,
// good: atomic when the flag stands alone (not coupled with other guarded state)
inserted: AtomicBool,   // store(Release) / load(Acquire)
```
*tier: cold | detector: opt-in `cargo xtask audit-clippy` (`mutex_atomic`) | present in kithara (kithara-ffi `item.rs`)*

## Async/tokio deep

**Blocking syscalls / heavy CPU on the async worker** (anchor; `E2` folds here)
```rust
// bad: heavy CPU pinned on the shared runtime
tokio::spawn(async move { let pcm = decode_track(src); });
// good: decode/resample on an owned RT thread; bound one-shot blocking in spawn_blocking
let decoder = spawn_blocking(move || build_decoder(src)).await?;
// the produce/decode loop lives on the dedicated core, never a tokio worker
```
*tier: hot | detector: enforced ast-grep `arch.no-raw-no-block` + rtsan-async + poll-budget census | already-enforced*

**Unbounded try_recv drain starving siblings**
```rust
// bad: unbounded drain hogs the worker
while let Ok(x) = rx.try_recv() { heavy(x); }
// good: bound to control-plane volume, keep the body trivial (kithara-hls peer eviction)
while let Ok(key) = self.eviction_rx.try_recv() { self.evict(key); }
```
*tier: warm | detector: no_block poll-budget census (partial) / manual | present in kithara (bounded, low-risk)*

**Oversized future state (large values across `.await`)**
```rust
// bad: owned buffer captured across await bloats the future
async fn push(&self, buf: Vec<f32>) { self.sink.send(buf).await; }
// good: pass a cheap handle; move big buffers into spawn_blocking closures, not futures
async fn push(&self, buf: Bytes) { self.sink.send(buf).await; }
```
Drop large locals before the first await; `Box::pin` only at spawn/recursion boundaries.
*tier: hot | detector: opt-in `cargo xtask audit-clippy` (`large_futures`) | preventive*

**`select!` loop recreating stateful futures each turn**
```rust
// bad: fresh sleep()/future rebuilt every turn loses timer state
loop { select! { _ = sleep(dt) => tick(), m = rx.recv() => on(m) } }
// good: hoist with interval/tokio::pin!, add biased where fairness is wasted
let mut ticks = interval(dt);
loop { select! { biased; _ = ticks.tick() => tick(), m = rx.recv() => on(m) } }
```
Keep only cancel-safe ops in `select!`.
*tier: warm | detector: manual | preventive (discipline already followed: kithara-hls `peer.rs` biased + hoisted arms)*

**`tokio::spawn` per tiny item in a loop**
```rust
// bad: one task per micro-CPU item
for x in items { tokio::spawn(async move { tiny(x) }); }
// good: one long-lived worker fed by bounded mpsc; batch with recv_many
let n = rx.recv_many(&mut buf, 32).await;   // one wakeup drains up to 32
for x in buf.drain(..n) { tiny(x); }
```
One spawn per network fetch stays fine - that is bounded IO concurrency, not per-item CPU.
*tier: warm | detector: ast-grep `arch.no-raw-tokio-task` (routes spawns, doesn't detect per-item) / manual | preventive*

**Unbounded channel without backpressure** (`L11` folds here)
```rust
// bad: unbounded producer, no backpressure
let (tx, rx) = mpsc::unbounded_channel::<ResourceKey>();
// good: bounded to the buffering target -> send().await throttles the producer
let (tx, rx) = mpsc::channel::<ResourceKey>(cap);
// pick by intent: watch=latest-state, mpsc=queue, oneshot=RPC reply, broadcast=fan-out
```
Reserve `unbounded_channel` for provably rate-limited producers and justify it in a comment; at the RT edge use a fixed-capacity SPSC ring.
*tier: warm | detector: manual (backlog `unbounded_channel(` census) | present in kithara (kithara-hls `stream/hls.rs`, kithara-stream `downloader.rs` - control-plane, uncommented)*

**`tokio::sync::Mutex` for plain data** (across-await half is `E1`)
```rust
// bad: async mutex guarding plain data, or a std guard held across await
data: tokio::sync::Mutex<State>,
// good: sync platform mutex, lock-mutate-drop before any await
data: kithara_platform::sync::Mutex<State>,   // async mutex only if the section spans .await
```
*tier: warm | detector: clippy `await_holding_lock` + devtools `await_under_guard` | already-enforced*

**Thin async wrapper (needless state machine)**
```rust
// bad: fresh async state machine wrapping a single await
async fn get(&self, k: K) -> V { self.inner.get(k).await }
// good: forward the callee's future (std::future::ready() for constants)
fn get(&self, k: K) -> impl Future<Output = V> + '_ { self.inner.get(k) }
```
*tier: cold | detector: enforced ast-grep `rust.no-thin-async-wrapper` | already-enforced*

**Watch-for (absent in kithara, no example):**
- **`buffer_unordered`/`FuturesUnordered` with a heavy consumer body** - a fat `for_each` serializes in-flight IO; spawn real tasks (`JoinSet` + permits), keep the body trivial. *tier: warm | detector: none | preventive*
- **`#[instrument]` on per-poll / per-chunk fns** - span setup on every call; instrument at task/segment granularity with `skip_all`, use counters for rates. *tier: warm | detector: none | preventive*
- **`Runtime::new().block_on(...)` per call** - rebuilds the runtime each time; hold one long-lived `Handle` and block only at the true FFI boundary (kithara uses a shared `FFI_RUNTIME`). *tier: cold | detector: none | preventive*
- **`span.enter()` held across `.await`** - corrupts trace nesting; use `.instrument(span)` on the future. *tier: warm | detector: none | preventive*
- **`block_in_place` as a default blocking escape hatch** - prefer bounded `spawn_blocking` or an owned thread with channel handoff; structurally blocked by the tokio-routing rules. *tier: warm | detector: enforced ast-grep `arch.no-raw-tokio-task` | already-enforced*

## Float / DSP

**Blind `mul_add` across targets** - `mul_add` is a true hardware FMA only on aarch64/Apple; on wasm32 (no scalar FMA opcode) and baseline x86 (SSE2; `+avx2` does not imply `+fma`) it lowers to a libm `fma()` *call* - slower, not faster.

```rust
// bad: on wasm32 / baseline x86 this becomes a libm fma() call per sample
let y = b0.mul_add(x0, b1 * x1 - a1 * y1);
// good: keep mul_add on aarch64; on the scalar wasm/x86 path write plain FMA-contractable math
let y = b0 * x0 + b1 * x1 - a1 * y1; // or guarantee -C target-feature=+fma
```

`powf(2.0)`->`powi(2)` and `f32::hypot` are already used correctly. *tier: hot | detector: manual (clippy `suboptimal_flops`/`imprecise_flops` are nursery, not enabled) | present in kithara (resampler ScalarBiquad)*

**Bounds-checked index in a sample loop**

```rust
// bad: for i in 0..src.len() { dst[i] = src[i] * gain; }
// good: single-slice pairs -> zip; the house form in the biquad
for (s, d) in src.iter().zip(dst.iter_mut()) { *d = *s * gain; }
```

Parallel-array `for i in 0..n-1 { lp[i]/lps[i]/hps[i] }` (eq.rs) is not `needless_range_loop`-lintable and iterators don't express it cleanly - leave it. *tier: hot | detector: clippy `needless_range_loop` (deny) | already-enforced (applied in W1/W4)*

**Per-sample divide by a loop invariant**

```rust
// bad: one idiv per frame
let mono = sum / channels as f32;
// good: hoist the reciprocal once (beat/analyzer.rs:112, waveform inv_channels)
let inv = 1.0 / channels as f32;
let mono = sum * inv;
```

*tier: hot | detector: manual | present in kithara (universally followed)*

**Raw `as` float<->int cast in a hot loop**

```rust
// bad: silent truncation / sign loss, cast repeated per sample
let n = (x * 32768.0) as i16;
// good: hoist/validate; kithara prefers num_traits ToPrimitive
let n = (x * 32768.0).to_i16().unwrap_or(i16::MAX);
```

*tier: hot | detector: clippy `cast_lossless`/`cast_possible_truncation`/`cast_sign_loss`/`cast_precision_loss` (warn) | already-enforced*

## SIMD / vectorization

**`#[async_trait]` boxing on a hot path**

```rust
// bad: boxes a future every call - wrong on an RT/decode path
#[async_trait] trait Fetch { async fn get(&self, u: &str) -> Bytes; }
// good: native async fn in trait (RPITIT, 1.75) - static dispatch, no per-call box
trait Fetch { fn get(&self, u: &str) -> impl Future<Output = Bytes>; }
```

kithara's only `#[async_trait]` is in kithara-net (per-request, I/O-dominated - box cost is negligible there). *tier: warm | detector: manual | present in kithara (benign, net only)*

Watch for: **runtime SIMD feature-dispatch inside the inner loop** (`is_x86_feature_detected!` re-checked per sample) - cannot occur here; kithara delegates SIMD to Accelerate/vDSP (cfg) and rten-simd/bungee, no Rust-level `#[target_feature]`/`#[multiversion]`.

**Per-buffer dynamic FFI crossing on the audio path**

```rust
// bad: a wasm-bindgen / dynlib call per buffer on the RT chain
for chunk in stream { js_sink.write(chunk); }
// good: keep decode->DSP->resample->sink in one Rust link unit; FFI = coarse control + bulk data only
```

Already the design: Accelerate is statically linked (vDSP, not a per-buffer seam); uniffi is control-plane. *tier: warm | detector: manual | already-followed*

**Cold branch / side-effect in the DSP kernel** (folds P4+P6)

```rust
// bad: log/alloc mid-kernel blocks autovectorization (and may alloc on RT)
for x in block { if x.abs() > 1.0 { warn!("clip {x}"); } out.push(f(x)); }
// good: branch-free kernel; outline the cold report, check once per block
let peak = block.iter().fold(0.0f32, |p, &x| p.max(x.abs()));
if peak > 1.0 { report_clip(peak); } // #[cold] #[inline(never)]
```

An early `return 0.0` at sample entry for silence/bypass is *correct* (avoids denormals) - do not rewrite it into a full-block scan. *tier: hot | detector: manual (backlog `audit.tracing-in-loop-hot`, `audit.alloc-in-loop`) | present in kithara (currently clean; timestretch `warn!` is block-level)*

**IIR feedback without denormal flush**

```rust
// bad: biquad tail rings into subnormals on silent input -> per-op stall, no FTZ, no clamp
y = 0.999 * y + b0 * x;
// good: set FTZ+DAZ once on the RT thread (aarch64 FPCR FZ / x86_64 MXCSR),
//       AND/OR clamp state below a threshold; keep the silence fast-path that freezes state
```

wasm32 has *no* FTZ mechanism and Apple aarch64 already handles subnormals better - measure per target, prefer algorithmic suppression. *tier: hot | detector: manual | present in kithara (eq.rs DirectForm1/LR4, resampler ScalarBiquad state)*

**Modulo over a runtime length in a ring index**

```rust
// bad: % over a Vec::len() the compiler can't prove is 128 -> idiv per sample
pos = (pos + 1) % history.len();
// good: power-of-two capacity, mask the compile-time const
const LEN: usize = 128; // already pow2
pos = (pos + 1) & (LEN - 1);
```

*tier: hot | detector: manual (backlog `audit.modulo-index` misses hoisted `% var`) | present in kithara (eq.rs:487,516 bypass history)*

**Serial float reduction that won't vectorize** - LLVM cannot reassociate an `f32` add chain, so a naive `sum` stays scalar.

```rust
// bad: single serial fadd chain over a whole window
let energy: f32 = samples.iter().map(|s| s * s).sum();
// good: algebraic_add licenses reassociation (Rust 1.98) -> autovectorizes
let energy = samples.iter().fold(0.0f32, |a, &s| a.algebraic_add(s * s));
```

Runs in offline beat/waveform analysis, not the RT callback. *tier: hot | detector: manual (backlog `audit.float-sum-hot`, needs a length filter) | present in kithara (beat/analyzer.rs:465)*

Watch for: **horizontal `reduce_add` inside the loop** - only exists in hand-written explicit SIMD, which kithara has none of; accumulate vertically, reduce once after the loop if that ever changes.

**`*_fast` fast-math intrinsics** - these produce poison (genuine UB) on NaN/Inf, which real audio hits (codec glitches, envelope-divide-by-zero, unvalidated network streams).

```rust
// bad: UB on NaN/Inf
let s = unsafe { core::intrinsics::fadd_fast(a, b) };
// good: algebraic_* - same reassociation/FMA/reciprocal freedom, worst case a differently-rounded value, never UB
let s = a.algebraic_add(b);
```

Also ban the `fast_fp` / `fast-floats` crates. *tier: hot | detector: manual (trivial ast-grep candidate: `*_fast` intrinsics + Cargo deps) | preventive*

**Blanket `#[inline]` at the crate boundary**

```rust
// bad: #[inline] on every cross-crate fn -> icache + compile-time bloat
// good: rely on release fat LTO (lto="fat", codegen-units=1, already configured);
//       add #[inline] only to profiled tiny boundary fns (Deref/AsRef/From, sample converters)
```

*tier: warm | detector: manual | already-enforced (fat-LTO backstop; opt-level="z" is the deliberate iOS-size tradeoff)*

**DSP sized to a baked-in callback block**

```rust
// bad: breaks on a smaller/odd device period
fn callback(out: &mut [f32; 512]) { decode_and_process(out); }
// good: decouple device callback from processing via a ring buffer, size state to a declared max
//       (kithara: audio worker produces PCM via Node tick() scheduler; sink drains a ring)
```

*tier: hot | detector: manual | already-followed (decoupled worker/ring model)*

## Cache/layout/data-oriented

**Array-of-structs scanned for one field** (folds K4 fat-ids, K5 pointer-chasing)
```rust
// bad: whole struct dragged through L1 to read one field; String/PathBuf inline, per-sample hop
struct Voice { env: [f32; 5], meta: String, path: PathBuf, gain: f32 }
for v in &voices { sum += v.gain; }
// good: hot fields contiguous (SoA), cold metadata in a side table keyed by u32 id
struct Voices { gain: Vec<f32>, meta: Vec<VoiceMeta> }
for &g in &voices.gain { sum += g; }
```
For DSP keep planar per-channel buffers (`Vec<Vec<f32>>`, iterated per channel, one hop) and interleave only at the device boundary via `fast-interleave` - as bungee/resampler already do. Never `Vec<Box<T>>`/`Arc<Mutex<Box>>` chains in an inner loop.
*tier: hot | detector: manual | present in kithara (planar buffers, u32 ids)*

**Cloning shared id strings into every event/error/key** (folds K15)
```rust
// bad: each event/error/cache key owns a fresh copy of the same id
struct Track { url: String }
let ev = TrackEvent { url: self.url.clone() };       // heap String per event
// good: share one allocation
struct Track { url: Arc<str> }
let ev = TrackEvent { url: Arc::clone(&self.url) };  // refcount bump
```
kithara-events already threads `Arc<str>` `item_id`/`src`; the fix is `Arc<str>`, not a new ustr/lasso dep. For many freshly-created short values `CompactString` inlines <=24B, but prefer `Arc<str>` sharing for ids.
*tier: warm | detector: manual | present in kithara (partly adopted)*

**Default SipHash for trusted internal keys**
```rust
// bad: DoS-hardened SipHash on a hot map keyed by our own ids
let by_root: HashMap<String, LruEntry> = HashMap::new();
// good: Fx/aHash for trusted keys; keep SipHash only for untrusted external input
type FxHashMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
let by_root: FxHashMap<String, LruEntry> = FxHashMap::default();
```
Profile first (String keys, hashing may not be hot); new hasher dep needs workspace justification.
*tier: warm | detector: manual (backlog audit.default-hashmap-hot.yml) | preventive*

**False sharing of independently-written hot atomics**
```rust
// bad: producer and consumer thrash adjacent fields on one cache line
#[repr(C)] struct Rt { write_pos: AtomicU64, read_pos: AtomicU64 }
// good: pad each hot field to its own line (or keep them in separate Arcs, as kithara does)
struct Rt { write_pos: CachePadded<AtomicU64>, read_pos: CachePadded<AtomicU64> }
```
Only bites once two threads write adjacent fields; confirm with `perf c2c`. kithara wraps each atomic in its own `Arc`, so no packed site exists yet.
*tier: hot | detector: manual | preventive*

**Queue used to publish latest-wins state**
```rust
// bad: gain/playhead pushed through mpsc; consumer drains a stale backlog
gain_tx.send(new_gain)?;                 // N queued, only the last matters
// good: latest-wins snapshot
gain.store(new_gain, Relaxed);           // or watch::Sender / triple_buffer / ArcSwap
```
Keep queues for event streams where every element must be processed (kithara-events broadcast); use atomics/`ArcSwap` for scalar RT state.
*tier: warm | detector: manual | present in kithara*

**Lazy init / cold pages faulted on the first callback**
```rust
// bad: first RT callback pays LazyLock init + first page fault
fn process(&mut self, out: &mut [f32]) { let t = TABLE.get_or_init(build); /* ... */ }
// good: force it during prepare(), off the deadline, and pre-touch RT buffers
fn prepare(&mut self) { LazyLock::force(&TABLE); self.scratch.resize(max, 0.0); }
```
*tier: hot | detector: manual (arch.no-raw-no-block governs RT discipline) | preventive*

**SmallVec as the default "fast Vec" in hot structs**
```rust
// bad: SmallVec everywhere; spill branch + fat inline storage on every move
per_channel: SmallVec<[PcmBuf; 8]>,
// good: hard max -> ArrayVec (no data-ptr branch); reusable scratch -> Vec + clear()
per_channel: ArrayVec<PcmBuf, 8>,        // channels <= 8 is a real bound
scratch.clear(); scratch.extend(frame); // pooled/reused, not re-allocated
```
kithara's `SmallVec<[PcmChunk;2]>` for transient 0-2 returns is deliberate; reach for SmallVec only on profiled churn, not by default.
*tier: hot | detector: manual | present in kithara (deliberate uses)*

**Runtime-length loop for a compile-time-fixed DSP kernel**
```rust
// bad: biquad coeffs as a runtime slice -> bounds checks, no unrolling
fn step(x: f32, c: &[f32]) -> f32 { c[0]*x + c[1]*/* .. */ }
// good: size in the type, destructure once (register-resident)
let [b0, b1, b2, a1, a2] = self.coefficients;   // [f32; 5]
b0.mul_add(x0, b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2)
```
*tier: hot | detector: manual | present in kithara (resampler biquad)*

**HashMap for a tiny / ordered-access closed set** (folds K20)
```rust
// bad: HashMap for the 3-5 HLS variants or a bounded codec table
let variants: HashMap<VariantId, VariantInfo> = /* .. */;
// good: contiguous Vec + linear find (or binary_search / enum-indexed [V; N])
let variants: Vec<VariantInfo> = /* .. */;
variants.iter().find(|v| v.id == want)   // scan of 3-5 beats hashing + pointer chase
```
`BTreeMap` only for genuine range/ordered access.
*tier: warm | detector: manual | present in kithara (Vec<VariantInfo>)*

**`as_chunks`/`chunks_exact` expected to unroll a runtime block**
```rust
// bad: assuming unrolled codegen from a runtime-sized chunk
for blk in buf.chunks(block) { process(blk); }   // block: usize (runtime)
// good: compile-time N unlocks unrolling; runtime lengths correctly stay on .chunks()
let (blocks, tail) = buf.as_chunks::<128>();
for blk in blocks { process(blk); }
```
kithara's runtime-length timestretch/beat blocks legitimately keep `.chunks()`.
*tier: hot | detector: manual | present in kithara*

**Blanket `#[inline(always)]` sprinkled for speed**
```rust
// bad: forced inlining bloats code, pressures I-cache, can pessimize
#[inline(always)] fn mix(a: f32, b: f32) -> f32 { a + b }
// good: trust LTO for cross-crate inlining; add #[inline] only on a profiled hot miss
fn mix(a: f32, b: f32) -> f32 { a + b }
```
kithara's 14 `inline(always)` are all inert no_block/test markers that must inline - a justified exception.
*tier: hot | detector: manual | preventive*

**Cold format/tracing built inline in a per-sample loop**
```rust
// bad: unconditional fmt/tracing in the hot body
for x in samples { tracing::trace!(?x); out.push(dsp(x)); }
// good: keep the body fmt/alloc-free; format lives on a #[cold] error branch
for x in samples { out.push(dsp(x)); }
```
LLVM cold-outlines error construction, but only while it stays on a rare branch.
*tier: hot | detector: manual | preventive*

Watch-for (antipattern absent in kithara today):
- **Hand-zoned cache fields defeated by `repr(Rust)`** - if you ever hand-order fields into cache zones, pin with `#[repr(C, align(128))]`; default `repr(Rust)` reorders and silently undoes it. *tier: hot | detector: manual | watch-for*
- **`get_unchecked` to skip bounds checks** - never in network-fed decode paths; narrow to sub-slices / `try_into` fixed arrays instead. *tier: hot | detector: clippy undocumented_unsafe_blocks=deny | watch-for*
- **Manual `_mm_prefetch` on linear/strided access** - hardware prefetchers cover stride-1; prefetch only for irreducibly irregular access with measured distance. *tier: hot | detector: manual | watch-for*
- **Hot-cache microbench illusion** - warm-L1 fixtures overstate headroom; rotate a pool larger than LLC and validate rankings on the real pipeline (see `test-harness.md`). *tier: hot | detector: manual | watch-for*

The task's format spec differs from the current file's prose style; I'll follow the task's compact catalog format as instructed. Here is the dense markdown for my section.

## Allocators, arenas, and ownership

**Per-packet buffer allocation in the streaming loop**
```rust
// bad: fresh buffer per packet churns the long-lived heap
loop {
    let mut buf = Vec::with_capacity(SEG);
    fill(&mut buf, packet); decode(&buf);
}
// good: lease from the pool, clear + refill, returns on drop
loop {
    let mut buf = pool.get();      // kithara-bufpool, fixed size class
    buf.clear();
    fill(&mut buf, packet); decode(&buf);
}
```
Segregate lifetimes: recurring buffers on pools (chosen over `thread_local` for cross-task ownership), parse-and-discard scratch on a single-thread `thread_local! RefCell<Vec>` with `clear()`-on-entry; keep churn off long-lived caches. *tier: warm | detector: perf.prefer-byte-pool + perf.prefer-pcm-pool + perf.no-collect-iter-roundtrip (enforced ast-grep) | present in kithara*

**Media payloads copied instead of ref-counted**
```rust
// bad: deep-copies the segment to share/slice it
let payload: Vec<u8> = resp.to_vec();
cache.put(payload.clone());            // full copy
decode(&payload[range].to_vec());      // another copy
// good: refcount bump + O(1) alloc-free view
let payload: Bytes = resp;             // BytesMut::freeze() at the producer
cache.put(payload.clone());            // bump
decode(payload.slice(range));          // zero-copy slice
```
Hand payloads across fetch->cache->decode as `bytes::Bytes`; share immutable IDs/URLs as `Arc<str>`. Stray `to_vec` remains only in cold FFI spots (symphonia demuxer, android CSD). *tier: warm | detector: manual | present in kithara*

**Arc clone / `load_full()` in the RT block loop**
```rust
// bad: atomic refcount bump every audio block
loop { let cfg = shared.load_full(); render(&cfg, block); }
// good: clone once at setup; Guard-load per block; drop off-RT
let cfg = shared.clone();              // one bump, off the hot loop
loop {
    let g = params.load();             // ArcSwap Guard, no refcount bump
    render(&cfg, &g, block);
}                                       // last Arc drop -> trash_tx, off the RT thread
```
Pre-warm the ArcSwap debt node before playback so the first store does not allocate on the RT thread. *tier: hot | detector: manual (blocking subset caught by rtsan / no_block lanes) | present in kithara*

**Arc as ownership glue for single-owner values**
```rust
// bad: per-field Arcs with no actual sharing
struct Deck { track: Arc<Track>, meta: Arc<Meta> }
// good: one owner, borrows down; one coarse Arc only where truly shared
struct Deck { track: Track, meta: Meta }
fn render(deck: &Deck) { /* &-borrow */ }
```
Use snapshot/command models across threads, not per-field Arcs. `Rc<RefCell<Node>>` parent/child webs do not occur here; a real graph should use slotmap index handles or petgraph, and `Rc<Box<_>>` is caught by default clippy. *tier: warm | detector: AGENTS.md red-flag gate + arch.no-arc-mutex-godmap/collection (enforced ast-grep) + clippy redundant_allocation | already-enforced*

**Transient peak pins wasm linear memory**
```rust
// bad: whole-segment collect + worst-case scratch on a wasm path
let pcm: Vec<f32> = decode_all(seg).collect();   // peak is permanent
// good: stream through a pre-warmed pooled buffer
let mut buf = pool.get();                         // pre-warmed at init
for frame in decode(seg) { sink.push(&mut buf, frame); }
```
WASM `memory.grow` is one-way, so every transient peak stays resident. Pre-warm the dlmalloc `thread_local` at init; never `wee_alloc`. *tier: warm | detector: manual | preventive (kithara ships a wasm build)*

**Global-allocator swap without a target benchmark**
```rust
// bad: cargo-culted, unbenchmarked
#[global_allocator] static A: Mimalloc = Mimalloc;
// good: no swap unless a per-target bench (p99 decode-loop + phys_footprint) earns it
```
kithara ships the system allocator; RT lanes stay allocation-free regardless of the choice. Keep any swap per-target in one place, benched against real segment decode. *tier: cold | detector: manual | preventive (no global-allocator swap present)*

**Allocating error / Display path**
```rust
// bad: eager format + join in Display for a maybe-unused error
let e = Error::Msg(format!("bad segment {id}"));
// good: thiserror variant + lazy context on the error branch only
#[derive(thiserror::Error)] enum E { #[error("bad segment {0}")] Seg(u32) }
res.with_context(|| format!("segment {idx}"))?;
```
Model expected conditions as enum variants/`Option`, not boxed strings; kithara standardizes on thiserror (snafu rejected). *tier: warm | detector: rust.no-inherent-to-string + rust.no-to-string-method (enforced ast-grep) + clippy to_string_trait_impl | already-enforced*

**Watch-for (not present in kithara, no example needed)**
- **Boxed node graph instead of arena** - per-node `Box`/`Rc` object soup in a phase-scoped tree: use bumpalo (Copy/POD) or typed-arena (runs Drop), scoped to the phase. No such structure exists here. *tier: warm | detector: manual*
- **`Rc<RefCell<Node>>` / `Arc<RwLock<>>` pointer web as data model** - use slotmap index handles or owner+IDs. Only config-level `Arc<RwLock<..>>` exists, already governed by arch.no-arc-mutex-godmap. *tier: warm | detector: enforced ast-grep*
- **serde owned `String`/`Vec` for borrowable input** - deserialize with `&'de str` / `#[serde(borrow)] Cow<'de, str>` and keep the source `Bytes` alive. kithara uses rkyv + a custom HLS parser, so no serde-borrow path. *tier: warm | detector: manual*
- **mimalloc MADV_FREE inflates Apple RSS** - only if mimalloc is ever adopted on Apple: pin >= 3.1.4 or `MIMALLOC_PURGE_DECOMMITS=1`, and measure phys_footprint, not RSS. No mimalloc dependency today. *tier: cold | detector: manual*
- **Manual zeroing loop defeats zero-pages** - `vec![0.0f32; n]` (lowers to `alloc_zeroed`) for fresh buffers, `buf.fill(0.0)` on the live slice for reused RT buffers; never a hand-rolled zero loop. Existing `push(0)` sites are single-element padding, not zero-fills. *tier: hot | detector: manual*
- **dhat heap-count assertions in the parallel libtest harness** - if a no-alloc RT contract test is adopted, put each `dhat::assert!` in its own integration-test file (own process), since libtest runs tests in parallel. dhat is not currently used. *tier: warm | detector: manual*

## Dispatch & compile-time

**Per-sample virtual dispatch** - hoist `dyn`/`Box<dyn>` calls to block or track granularity so the concrete impl's inner loop can inline and vectorize.
```rust
// bad: one vtable call per sample
for x in buf.iter_mut() { *x = effect.process_sample(*x); }
// good: one vtable call per block, monomorphic loop inside the impl
for effect in &mut self.effects { effect.process(buf); } // Vec<Box<dyn AudioEffect>>
```
Closed sets -> `enum` + `match`; keep `dyn` only where the backend is runtime-selected (`Box<dyn Decoder>` per track, `Box<dyn StretchBackend>` per stream).
*tier: hot | detector: manual (dyn-in-hot-loop rule proposed, not wired) | present in kithara (Decoder/AudioEffect/StretchBackend all dispatch coarsely)*

**Monomorphization bloat** - split a large generic fn so a tiny type-parametric shell delegates to one non-generic core; only the shell is duplicated per instantiation.
```rust
// bad: whole 500-line body monomorphized for every <D, C>
fn run<D: Demux, C: Codec>(d: D, c: C) -> Pcm { /* 500 lines */ }
// good: thin generic shell, one core compiled once
fn run<D: Demux, C: Codec>(d: D, c: C) -> Pcm { run_core(&mut erase(d, c)) }
#[inline(never)] fn run_core(io: &mut dyn DecodeIo) -> Pcm { /* 500 lines */ }
```
Relevant only to the iOS-size axis and only after `cargo llvm-lines` / `cargo bloat --release` (justfile exposes bloat) prove a specific generic dominates.
*tier: hot | detector: manual (opt-in census, unmeasured) | preventive*

**Recomputing statics per call** - cache registries/tables once, don't rebuild them on the hot path.
```rust
// bad: rebuild the codec registry every call
fn registry() -> CodecRegistry { CodecRegistry::build() }
// good
static CODEC_REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
fn registry() -> &'static CodecRegistry { CODEC_REGISTRY.get_or_init(CodecRegistry::build) }
```
*tier: hot | detector: manual | present in kithara (symphonia `CODEC_REGISTRY: OnceLock`)*

**`Box<dyn T>` return where `impl Trait` fits** - watch for boxing a single concrete return type; absent here - every `Box<dyn>` return (`build_backend`, `create_decoder`) is a runtime-selected, *stored* trait object that `impl Trait` cannot express. *tier: hot | detector: opt-in `cargo xtask audit-clippy` (`unnecessary_box_returns`, pedantic) | already clean*

## Build/codegen/LTO

**Size profile starves DSP autovectorization** (LIVE) - the workspace ships `opt-level = "z"`, which disables loop autovectorization and lowers inline thresholds; with no SIMD crate, autovectorization is the *only* vector path, so the iOS/wasm DSP ships scalar.
```toml
# bad: workspace-wide in [profile.release] and [profile.wasm-release]
opt-level = "z"
# good: keep "z" for cold orchestration crates, speed-opt the hot DSP crates
[profile.release.package.kithara-resampler] # + kithara-decode/-stretch/-audio/-beat
opt-level = 3
```
Verify kernels vectorize with `cargo-show-asm`. Cargo caveat: a per-package override sets `opt-level` but *not* `lto`/`panic`.
*tier: hot | detector: manual (Cargo.toml census) | present in kithara (no `[profile.release.package.*]` overrides exist)*

**wasm ships scalar** (pairs with the above) - no wasm lane enables `+simd128` and the trunk `wasm-opt` params omit `--enable-simd`.
```toml
# bad: wasm32 rustflags = +atomics,+bulk-memory,+mutable-globals   (no SIMD)
# good
rustflags = ["-C", "target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128"]
```
Also add `--enable-simd` to the `wasm-opt` params. Only pays off *with* the opt-3 fix above (opt-z otherwise defeats the autovectorizer).
*tier: hot | detector: manual (build-config census) | present in kithara (.cargo/config.toml + kithara-ffi index.html)*

**Benchmarking a binary nobody ships** - no `[profile.bench]` is defined, so `cargo bench` runs opt-3 / LTO-off / cgu=16 while the shipped artifact is opt-z / fat-LTO / cgu=1.
```toml
# good: pin the bench profile to the shipping knobs, then assert it in CI
[profile.bench]
opt-level = "z"   # match release
lto = "fat"
codegen-units = 1
debug-assertions = false
overflow-checks = false
```
At opt-3 the `tests/benches/perf_audit.rs` gate can never surface the opt-z vectorization loss above.
*tier: n/a | detector: manual (Cargo.toml census) | present in kithara (no `[profile.bench]`)*

**Distributed `target-cpu=native`** - never on a shipped binary; pin an explicit floor matching the deploy target instead.
```toml
# bad: -C target-cpu=native   -> SIGILL on older CPUs in the field
# good: -C target-cpu=apple-a14   (matches the iOS deployment floor)
```
kithara ships none (good). x86_64 is desktop-macOS only; if that DSP ever matters, add AVX2/FMA *runtime dispatch* (multiversion/pulp) at kernel entry rather than a global `target-cpu`. Covers O9 + O10.
*tier: hot | detector: manual | preventive (absent)*

**fat-LTO + cgu=1 cargo-cult** - release pins `lto="fat"` + `codegen-units=1` with no recorded thin-vs-fat A/B. Defensible for a shipped SDK, but measure once on the real perf suite (blocked by the bench-profile fix) and keep thin unless fat shows a reproducible win; reserve fat+cgu1 for tagged release artifacts. *tier: n/a | detector: manual | present in kithara (unmeasured)*

**Size-lane `build-std-features`** - iOS/wasm rebuild std with only `panic_immediate_abort`; the small remaining lever is adding `optimize_for_size` to the `-Z build-std-features` on those size lanes. *tier: cold | detector: manual | present in kithara (mostly handled)*

**Cross-crate calls without LTO** - watch for hot cross-crate boundaries not being whole-program-optimized; non-issue here, release already `lto="fat"` (wasm-release too). Add `#[inline]` to a specific tiny cross-crate fn only if profiling later flags that exact boundary. *tier: hot | detector: manual | already-enforced (self-covered)*

**Nightly `std::simd` / dead SIMD crates** - watch for `feature(portable_simd)`, `faster`, or `packed_simd`; absent. If explicit SIMD is ever added, use `wide` on stable (NEON + simd128 + SSE/AVX matches kithara's target matrix), not nightly `std::simd`. *tier: hot | detector: manual | preventive*

**Unexamined FFI panic strategy** - watch for default unwind crossing the uniffi/wasm surface; already decided: release `panic="abort"` + apple `panic_immediate_abort` + `unwrap_used=deny`. Do not rely on uniffi `catch_unwind` under abort. Covers O6 + O11. *tier: cold | detector: manual | already-enforced*

**Untuned release codegen** - watch for a shipped default profile; non-issue, `[profile.release]` is fully tuned (fat-LTO, cgu=1, `panic="abort"`, `strip="symbols"`, opt-z) - if anything too aggressive on the size axis (see the opt-z item). *tier: n/a | detector: manual | already-enforced*

**Unstripped mobile symbols** - watch for shipping symbols in the artifact; non-issue, release `strip="symbols"` + `debug=false` and apple strips slices. Keep archiving dSYM separately for crash symbolication. *tier: n/a | detector: manual | already-enforced*

**PGO/BOLT as a magic last mile** - watch for reaching for profile-guided opt before source fixes plateau; absent. If pursued, feed `cargo-pgo` real HLS/decode workloads (not unit tests); BOLT is ELF-only, so off the table for Mach-O iOS/macOS. *tier: n/a | detector: manual | preventive*

## Strings, logging & IO

**Allocating tracing / telemetry on the audio path**
```rust
// bad: `?`/`%` field args evaluate at every callsite even when TRACE is off
trace!(stats = ?compute_stats(&buf));
// good: gate the expensive field, or fold per-frame data into a relaxed counter
if tracing::enabled!(Level::TRACE) { trace!(stats = ?compute_stats(&buf)); }
self.frames.fetch_add(n, Relaxed);                 // UI polls the atomic

// bad: format! string / cloned Vec payload per decoded chunk
bus.publish(Event::Progress(format!("read {read}/{total}")));
// good: typed Copy enum through the bounded, deferred EventBus (drop-oldest on full)
bus.enqueue(ReadProgress { read, total });          // one per chunk, flushed later
```
Also floor out verbosity in ship builds via tracing's `release_max_level_info` (not set in any Cargo.toml today).
*tier: warm | detector: manual | present in kithara (EventBus enqueue/flush; hot loops already field-clean)*

**Premature itoa/ryu for control-rate numbers**
```rust
// bad: pull in itoa/ryu to format a once-per-request Range header
// good: control-rate Display is fine; reserve itoa/ryu for per-frame numeric IO (kithara has none)
impl fmt::Display for RangeSpec { /* ... */ }        // range.to_string() once per HTTP GET
```
*tier: warm | detector: enforced-ast-grep (rust.no-inherent-to-string) | already-enforced*

**Fresh, un-sized buffer per IO read**
```rust
// bad: grow from empty, then hand off
let mut buf = Vec::new();
while let Some(c) = stream.next().await { buf.extend_from_slice(&c); }
Bytes::from(buf)
// good: pre-size from the already-parsed Content-Length; zero-copy handoff
let mut buf = Vec::with_capacity(content_length);
// ...extend...
Bytes::from(buf)                                     // per-segment scratch -> kithara-bufpool
```
*tier: warm | detector: enforced-ast-grep (perf.prefer-byte-pool) | present in kithara (Bytes handoff; missing with_capacity in net/client body_bytes)*

Absent in kithara today - watch for regressions:
- **`BufRead::lines()` per-line String** - parse small manifests from one `str::from_utf8(data)` + borrowed line iteration; `lines()` allocates a String per line. *(kithara-hls parses borrowed.)*
- **`serde_json::from_reader` on a raw File** - use `from_str`/`from_slice` over in-memory bytes; if a file path appears, `fs::read` then `from_slice`. *(All JSON is `from_str` today.)*
- **Unbuffered byte-at-a-time IO** - container/ADTS parsing goes through symphonia's buffered `MediaSourceStream` over in-memory `Bytes`/mmap, never raw `File`/`TcpStream` per-field reads.
- **mmap cargo-cult for sequential reads** - mmap only for random-access index/cache behind the owning `AtomicChunked<MmapDriver>`; stream segments with sized reads/`Bytes`.
- **`println!` re-locking stdout in a loop** - dev-tooling `println!` is sanctioned (`rust.no-debug-prints` bans it in prod); in a chatty loop lock stdout once into a `BufWriter` and `writeln!`.
- **Vectored-IO / concat-before-write** - for header+payload writes prefer `bytes::Buf::chain` or a sized `BufWriter` over `[hdr, body].concat()`; check `is_write_vectored()` before trusting `write_vectored` (std default writes only the first slice).

## FFI & platform boundaries

**Chatty fine-grained FFI / wasm getters**
```rust
// bad: per-frame getter marshals a record/String across the boundary on every call
#[uniffi::export] fn now_playing(&self) -> TrackInfo { /* String+Vec via copying RustBuffer */ }

// good: coarse control-plane - opaque handles, scalar getters, one batched snapshot, push observer
#[uniffi::export] fn current_time(&self) -> f64 { ... }
#[uniffi::export] fn snapshot(&self) -> FfiPlayerSnapshot { ... }               // batched state
#[uniffi::export] fn set_observer(&self, obs: Arc<dyn PlayerObserver>) { ... }  // push, don't poll
// wasm-bindgen: same rule - numeric IDs, batch snapshots, DSP stays in the worker
#[wasm_bindgen] pub fn current_time_ms_js(&self) -> f64 { ... }
```
Why: UniFFI lowers every `String`/`Vec`/record/enum through a copying RustBuffer (UTF-8 per call); wasm-bindgen copies via TextEncoder/TextDecoder per crossing. The RT audio callback stays entirely in Rust behind opaque `Arc<AudioPlayerItem>` handles.
*tier: cold | detector: manual | present in kithara (facade scalars + snapshot + observer)*

Absent in kithara today - watch for regressions:
- **Mobile periodic-polling wakeup storm** - drive buffer-health/position/ABR from events, not independent `tokio::time::interval` polls; coalesce/stop timers when paused or backgrounded; route all timing through kithara-platform. *(Prevented by arch.no-direct-time / no-implicit-clock / no-implicit-sleep.)*

All three sections are in hand. Here is the dense markdown for the assigned sections.

## Benchmarking methodology

**Benchmark input by-value / result DCE'd**
```rust
// bad
c.bench_function("mix", |b| b.iter(|| mix(pcm)));            // by-value memcpy each iter; result can be optimized away
// good
c.bench_function("mix", |b| b.iter(|| mix(black_box(&pcm)))); // black_box the borrow, return the output
```
*tier: bench; detector: manual; present in kithara (tests/benches/perf_audit.rs already `black_box(&pcm)`)*

**Setup/drop timed inside `iter`**
```rust
// bad
b.iter(|| { let mut p = proc.clone(); p.run(&chunk); });    // clone + Drop counted in the timed region
// good
b.iter_batched_ref(|| proc.clone(), |p| p.run(&chunk), BatchSize::SmallInput); // excludes both
```
Use `iter_with_large_drop` for large outputs; never `BatchSize::PerIteration` for sub-us kernels.
*tier: bench; detector: manual; partial in kithara (stretch bench excludes setup, not drop)*

**Flamegraph of the stripped release**
```toml
# bad: profiling [profile.release] with strip="symbols", opt-level="z" -> truncated stacks
# good: dedicated profile, line tables only (no codegen impact)
[profile.profiling]
inherits = "release"
debug    = true
strip    = "none"
```
On macOS use samply/Instruments (inline-frame aware).
*tier: bench; detector: manual; gap in kithara (no `[profile.profiling]`)*

**Wall-time PR gate on shared CI**
```text
// bad: gate PRs on criterion wall-time on a shared VM (perf.yml on ubuntu-latest)
// good: keep wall-time opt-in/local (critcmp); hard-gate only on bare metal or one-shot iai-callgrind
```
*tier: bench; detector: manual; already decided in kithara (perf.yml.disabled, `RUN_BENCHMARKS=1` gate)*

**Trusting small deltas** - treat wall-time deltas below ~2x the measured noise floor as unproven; corroborate with repeated runs or layout-insensitive counts, set `noise_threshold` when gating. *tier: bench; detector: manual; preventive*

**icount without a cache model** - watch for: judging a memory-bound kernel by instruction count alone; kithara has no iai-callgrind (criterion wall-time only), so N/A today. *tier: bench; detector: manual; preventive*

**Truncated criterion sampling** - cut the *number* of benches, not `sample_size`; kithara's `sample_size(20)` is a deliberate opt-in-speed tradeoff, so a `.sample_size(small)` detector would only flag its own choice. *tier: bench; detector: manual; deliberate in kithara*

## Repo invariants

These are the dedup boundary: each is already an ERROR/deny gate. Entries exist to point at the enforcing rule, not to re-teach it.

**`Arc<Mutex<Collection>>` / god-map / `Arc<Atomic*>` glue**
```rust
// bad
tracks: Arc<Mutex<HashMap<TrackId, Track>>>,   // shared-mutable god-map as ownership glue
// good
// one owner holds the map; mutate via commands, expose read-side ArcSwap snapshots
```
*tier: warm; detector: `arch.no-arc-mutex-collection` + `arch.no-arc-mutex-godmap` (ast-grep); already-enforced*

**Fallback chain / sentinel value**
```rust
// bad
let v = primary().or_else(secondary).unwrap_or(SENTINEL);   // papers over a broken state contract
// good
let v = resolve()?;   // one correct source; fix the contract, not the symptom
```
*tier: warm; detector: `rust.no-fallback-*` + `rust.no-sentinel-*` (ast-grep); already-enforced*

**Direct time / sleep / rng**
```rust
// bad
std::thread::sleep(d); let t = std::time::Instant::now(); let r = rand::random();
// good
platform.clock().now(); platform.sleep(d).await; platform.rng().next_u64();
```
*tier: cold; detector: `arch.no-direct-time` + `arch.no-implicit-sleep` + `arch.no-implicit-rng` (ast-grep); already-enforced*

**Global pool accessor**
```rust
// bad
let buf = kithara_bufpool::pcm_pool().get();          // ambient global accessor
// good
let buf = self.pcm_pool.get_with(|b| b.clear());      // pool passed through config
```
*tier: hot; detector: `perf.no-global-pool-accessor` (ast-grep); already-enforced*

**`unwrap()` / `expect()` in production**
```rust
// bad
let n = map.get(k).unwrap();
// good
let n = map.get(k).ok_or(Error::Missing { key })?;    // or unwrap_or(default)
```
*tier: warm; detector: `clippy::unwrap_used`=deny + `rust.no-expect-bare-string` (ast-grep); already-enforced*

## Other / backlog

**Per-packet / per-segment buffer allocation (M2)**
```rust
// bad
for seg in segments { let buf = seg.bytes().to_vec(); decode(buf); }   // alloc per packet
// good
let mut buf = pool.get_with(|b| b.clear());
for seg in segments { buf.clear(); buf.extend_from_slice(seg.bytes()); decode(&buf); } // reuse; Bytes for zero-copy
```
*tier: warm; detector: `audit.alloc-in-loop` / `audit.push-in-loop` (backlog, port to `.config/ast-grep/perf.*` in M3); present in red-flags, not yet ast-grep-enforced*

**Alloc-on-pool-miss fallback (I9)**
```rust
// bad
let buf = pool.try_acquire().unwrap_or_else(|| Vec::with_capacity(n)); // RT stall + banned fallback
// good
let Some(buf) = pool.try_acquire() else { self.underruns += 1; return; }; // wait-free, failable, counted
```
Pre-fill the pool to a computed budget; allocate-on-miss only on non-RT lanes, and count misses.
*tier: hot; detector: targeted ast-grep candidate (M3) - specializes `rust.no-fallback-*`; preventive*

**`Arc` clone / `load_full()` in the inner loop**
```rust
// bad
for block in blocks { let cfg = self.cfg.load_full(); render(block, &cfg); } // refcount bump per block
// good
let cfg = self.cfg.load();          // ArcSwap Guard, no bump; or clone the Arc once at setup and borrow &T
for block in blocks { render(block, &cfg); }
```
kithara pre-warms the arc_swap debt node off-RT; no lint enforces this.
*tier: hot; detector: manual; present in kithara (clean, preventive)*

**Cargo-cult `get_unchecked`**
```rust
// bad
for i in 0..n { let x = unsafe { *buf.get_unchecked(i) }; }   // UB on crafted length = security bug
// good
for &x in &buf[..n] { /* ... */ }    // iterators/sub-slice/assert! first; get_unchecked only last-resort w/ SAFETY + bench
```
kithara decodes untrusted network audio; 0 sites today.
*tier: hot; detector: manual (census candidate); preventive*

**RT-thread backlog (I1/I4/I5/I7/I8)** - no async primitive, tracing, channel allocation, or last-`Arc` drop on the RT callback; wait-free ringbuf SPSC + atomics across the boundary, released heap handed back to a collector/bufpool lane. Full rule lives in the RT-audio callback section. kithara already uses ringbuf SPSC (not tokio/crossbeam mpsc) and its DSP crates are tracing-free; detector scope is the kithara-play processor, not a broad sweep. *tier: hot; detector: manual census (kithara-play processor) + `arch.no-raw-no-block` for the blocking subset; present in kithara (clean)*

**Feed threads not RT-promoted / not workgroup-joined (I2)** - promote kithara's owned decode/DSP-feed threads (kithara-audio worker/decoder.rs) to RT priority and, on Apple, join the CoreAudio `os_workgroup`; the device callback thread itself is firewheel/cpal-owned. Keep the RT thread count small and fixed. *tier: hot; detector: manual census (not ast-grep-able); audit target*

**Blocking / unbudgeted CPU on an async worker (L1/L3)** - long-lived CPU (decode) on owned threads, one-shot blocking via bounded `spawn_blocking`; never run unbudgeted CPU loops on tokio workers. *tier: warm; detector: `arch.no-raw-no-block` (ast-grep, ERROR) + `#[kithara::no_block]` poll-budget lane (PR#105); already-enforced*

**Non-lock-free `AtomicCell` / RT busy-spin (J1/J2)** - zero surface today (no `AtomicCell`, no `spin` dep); preventively assert `AtomicCell::<T>::is_lock_free()` in a const/test and remove the sharing (SPSC/triple-buffer) rather than spin. kithara's `compare_exchange` sites are bounded CAS in bufpool budget accounting, not busy-spin. *tier: hot; detector: manual (preventive ast-grep for the `spin` crate); preventive*

**Rc/Arc for borrow convenience** - single owner + `&`/`&mut`; snapshot/command across threads; one coarse `Arc` at the top, never per-field `Arc` glue or `Rc<Box<T>>`. *tier: warm; detector: `clippy::redundant_allocation` + red-flags arc-glue rule; already-enforced*

**Segment payload cloned as `Vec<u8>`** - hand payloads off as `bytes::Bytes` (`clone` = refcount bump, `slice(a..b)` = O(1), producer `BytesMut::freeze()`); never `buf[a..b].to_vec()` to share. Already the idiom (126 `Bytes` sites in stream/hls/net). *tier: warm; detector: manual; present in kithara (already solved)*

**Buffered stream hides serialization (L2)** - watch for: `buffer_unordered`/`FuturesUnordered` whose concurrency is throttled by a slow `for_each` body; spawn bounded `JoinSet` tasks so IO progresses independently. Zero surface today. *tier: warm; detector: manual; preventive*

**`#[instrument]` on a hot future poll (L4)** - watch for: `#[instrument]` on `poll_*`/per-chunk fns (fires every poll); instrument at task/segment/track granularity with `skip_all` + `Copy` fields. Zero surface today. *tier: warm; detector: manual; preventive*

**Size-hint-losing adapters in DSP inner loops** - watch for: `.chain()`/`.chunks()` inside a per-sample loop (lost `ExactSizeIterator`, refused fusion); split `.chain` into two sequential loops, use `chunks_exact(N)` for fixed blocks. kithara already uses `chunks_exact` where sizes are fixed; the lone `.chain` is a cold once-per-open magic-cookie build. *tier: hot; detector: manual; not present*

**Fixed kernel stored as runtime-length `Vec<f32>`** - watch for: FIR/biquad taps in a `Vec`/`Box<[f32]>` iterated per sample; put the size in the type (`&[f32; N]`, `fn fir<const N: usize>`, `as_chunks::<N>()`). Moot in kithara: DSP kernels live in external crates (biquad, rubato, bungee) or Apple vDSP. *tier: hot; detector: manual; not present*

**`Rc<RefCell<Node>>` pointer-web data model** - watch for: parent/child `Rc<RefCell<_>>` graphs; use an arena/slotmap with index handles or owner + IDs. Not present in kithara (the lone `Rc<RefCell<BuildState>>` is a single cold wasm dispatcher node). *tier: cold; detector: `arch.no-arc-mutex-*` (partial); not present*

**jemalloc page-size mismatch on arm64 (disputed)** - watch for: N/A, kithara ships no jemalloc/mimalloc (only wasm dlmalloc). If ever adopted on Apple Silicon, build with `lg-page=14` (16K). *tier: n/a; detector: manual; preventive*

**Ring capacity by round number (disputed)** - watch for: sizing SPSC rings to "fit in L1" folklore; size by burst tolerance + latency budget and measure. Rings exist (ringbuf crate in kithara-audio/kithara-play); the cache-monopolization mechanism is refuted. *tier: hot; detector: manual; preventive*
