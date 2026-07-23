# Benchmarking & methodology

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

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
