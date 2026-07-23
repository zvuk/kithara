# DSP math & data layout

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

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
