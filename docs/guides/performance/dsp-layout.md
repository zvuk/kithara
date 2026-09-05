# DSP math & data layout

*Tier legend and the rest of the family: [performance.md](../performance.md).*

## Float & DSP kernels

**`mul_add` is not portably an FMA.** It is a true hardware fused multiply-add
only on aarch64 and Apple; on wasm32 (no scalar FMA opcode) and on baseline x86
(SSE2, and `+avx2` does not imply `+fma`) it lowers to a libm fma *call* -
slower, not faster. Keep `mul_add` on aarch64 and write plain FMA-contractable
arithmetic on the scalar wasm and x86 paths, or guarantee the target feature.
`powf(2.0)` -> `powi(2)` and `hypot` are already used correctly.
*tier: hot | detector: manual (clippy's flop lints are nursery, not enabled) |
present in kithara (`backend::Filter`)*

**A serial float reduction will not vectorize; the fast-math escape is UB.**
LLVM may not reassociate an `f32` add chain, so a naive sum stays scalar. The
algebraic operators license reassociation, FMA contraction, and reciprocals with
no worse outcome than a differently-rounded value. The `*_fast` intrinsics and
the fast-float crates instead produce poison - genuine UB - on NaN or Inf, which
real audio hits through codec glitches, envelope divides, and unvalidated network
streams.

```rust
// bad: stays a single serial fadd chain; and never reach for fadd_fast
let energy: f32 = samples.iter().map(|s| s * s).sum();
// good
let energy = samples.iter().fold(0.0f32, |a, &s| a.algebraic_add(s * s));
```

Runs in offline beat and waveform analysis, not in the RT callback.
*tier: hot | detector: manual (a `*_fast` ban is a trivial ast-grep candidate) |
present in kithara (beat analyzer)*

**IIR feedback needs a denormal story.** A biquad tail rings down into subnormals
on silent input and every subnormal op stalls. Set FTZ and DAZ once on the RT
thread (aarch64 FPCR, x86_64 MXCSR) and clamp the state below a threshold, or
keep the silence fast path that freezes state. wasm32 has *no* FTZ mechanism and
Apple aarch64 already handles subnormals better, so measure per target and prefer
algorithmic suppression. *tier: hot | detector: manual | present in kithara
(`DirectForm1`, `backend::Filter` state)*

**Keep the kernel body branch-free and side-effect-free.** A log, an allocation,
or an inline `format!` mid-kernel blocks autovectorization and may allocate on
RT. Fold the condition over the block, then call one outlined `#[cold]` reporter.
LLVM cold-outlines error construction only while it stays on a rare branch. An
early return at sample entry for silence or bypass is *correct* - it avoids
denormals - and must not be rewritten into a full-block scan. *tier: hot |
detector: manual (backlog `audit.tracing-in-loop-hot`) | present in kithara
(clean; the timestretch warning is block-level)*

- **Bounds-checked index in a sample loop** - `zip` the two slices; that is the
  house form in the biquad. The parallel-array loop over three arrays in the
  equalizer is not expressible cleanly as an iterator and is left alone.
  *clippy `needless_range_loop`=deny*
- **Per-sample divide by a loop invariant** - hoist the reciprocal once and
  multiply. *manual; universally followed*
- **Raw `as` float/int cast in a hot loop** - silent truncation or sign loss,
  repeated per sample; kithara prefers `to_i16()` and the other `ToPrimitive`
  conversions with an explicit saturating fallback. *clippy `cast_lossless`,
  `cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss`*
- **Modulo over a runtime length in a ring index** - the compiler cannot prove
  the length, so it emits an integer divide per sample. Give the buffer a
  power-of-two capacity and mask a compile-time constant. *manual (backlog
  `audit.modulo-index` misses a hoisted modulo); present in the equalizer bypass
  history*
- **Runtime-length loop over a compile-time-fixed kernel** - coefficients behind
  a runtime slice cost bounds checks and refuse to unroll. Put the size in the
  type and destructure `coefficients` once so it stays register-resident.
  *present in kithara (resampler biquad)*
- **Expecting a runtime-sized chunk to unroll** - only a compile-time chunk width
  unlocks unrolling; a runtime length correctly stays on `chunks`. Likewise an
  adapter that drops `ExactSizeIterator` inside a per-sample loop refuses fusion:
  split a chained iterator into two sequential loops. kithara's timestretch and
  beat blocks are legitimately runtime-length, and its one chained iterator is a
  cold once-per-open cookie build.
- **DSP sized to a baked-in callback block** - a fixed array parameter breaks on
  a smaller or odd device period. Decouple the device callback from processing
  through a ring and size state to a declared maximum, as the audio worker and
  its sink already do. *already-followed*

Watch for **runtime SIMD feature dispatch inside the inner loop** and a
**horizontal reduce inside the loop** (accumulate vertically, reduce once after
it). Neither can occur today: kithara delegates SIMD to Accelerate/vDSP and to
rten-simd and bungee, and writes no explicit SIMD of its own.

Boxing and inlining belong to [dispatch-build.md](dispatch-build.md); the FFI
surface belongs to [io-ffi.md](io-ffi.md). Two kithara facts that live with
them: the only `#[async_trait]` is in kithara-net, where the per-request box is
negligible against I/O; and the 14 `#[inline(always)]` sites are inert no_block
and test markers that must inline, a justified exception to trusting fat LTO.

## Cache & data layout

**Array-of-structs scanned for one field.** Dragging a whole struct through L1 to
read one field - worse when it inlines a `String` or a path - is the default
layout mistake. Keep the hot fields contiguous and move cold metadata to a side
table keyed by a `u32` id. For DSP that means planar per-channel buffers,
iterated one channel at a time, interleaved only at the device boundary, as
bungee and the resampler already do. Never a `Box`ed or locked pointer chain in
an inner loop. *tier: hot | detector: manual | present in kithara (planar
buffers, `u32` ids)*

- **Default SipHash for trusted internal keys** - the DoS-hardened hasher earns
  nothing on a map keyed by our own ids; Fx or aHash suits trusted keys and
  SipHash stays for untrusted external input. Profile first (string keys may not
  be hot) and justify any new hasher dependency at the workspace level. *backlog
  `audit.default-hashmap-hot`; preventive*
- **`HashMap` for a tiny or ordered closed set** - a linear scan of the three to
  five HLS variants beats hashing plus a pointer chase; kithara stores
  `VariantInfo` in a `Vec`. `BTreeMap` only for genuine range or ordered access.
- **False sharing of independently written hot atomics** - a producer and a
  consumer writing adjacent fields thrash one cache line. Pad each hot field to
  its own line, or keep them in separate allocations as kithara does; confirm
  with a cache-to-cache profile before restructuring. *preventive: no packed site
  exists*
- **Lazy init and cold pages faulted on the first callback** - the first RT
  callback otherwise pays a `LazyLock` initialization and a page fault. Force the
  table and pre-touch the RT buffers during `prepare()`, off the deadline.
- **`SmallVec` as the default "fast `Vec`"** - it adds a spill branch and fat
  inline storage to every move. A hard bound belongs in `ArrayVec`, reusable
  scratch in a pooled buffer; reach for `SmallVec` only on profiled churn.
  kithara's `SmallVec<[AudioChunk; 2]>` for transient zero-to-two returns is a
  deliberate exception. *see [allocation.md](allocation.md) for pooled scratch*

Watch for **hand-zoned cache fields defeated by the default representation**
(pin the order with an explicit `repr` and alignment, or the compiler silently
undoes it), **`get_unchecked` used to skip bounds checks** (never on a
network-fed decode path - narrow to a sub-slice or convert into a fixed array
instead; zero sites today), and **manual prefetch on linear or strided access**
(hardware prefetchers cover stride-1; prefetch only irreducibly irregular access,
at a measured distance).
