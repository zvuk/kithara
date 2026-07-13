# Performance

Open this when optimizing a hot path, cutting allocations, or triaging a performance regression; beyond the red-flags gate, make changes only with a measurement.

## Allocation discipline

- Reuse buffers with `clear()` instead of creating them per iteration, and do not
  call `shrink_to_fit()` before refilling them. Shrink only at lifecycle boundaries
  to a policy watermark, as kithara-bufpool does with `shrink_to` (manual - no lint
  encodes the clear-versus-shrink rule).

```rust
// bad
let mut buffer = Vec::new();
// good
buffer.clear();
```

- Carry segment and packet payloads as `bytes::Bytes`, not `Vec<u8>` copied for
  sharing or slicing. Use `Bytes::slice` and freeze producers with
  `BytesMut::freeze()`, following the kithara-stream, kithara-hls, and kithara-net
  convention (manual - no lint encodes this).
- Share immutable asset IDs and URLs as `Arc<str>`, not repeatedly cloned `String`;
  the clone becomes a refcount bump, already the convention in kithara-events.
  Do not add an interning dependency (manual - no lint encodes this).
- On wasm-reachable paths, do not collect whole segments or size scratch buffers
  to a worst-case peak because linear memory does not shrink. Stream through fixed
  pooled buffers and pre-warm allocations at initialization; do not use `wee_alloc`
  (manual - no lint encodes this).

## Hot-path iteration and DSP math

- Use `sort_unstable_by(f32::total_cmp)` for analysis buffers when equal-value
  stability has no meaning; stable `sort_by` allocates scratch for a guarantee the
  analysis does not need (manual - the proposed detector was dropped as high-FP).
- Replace runtime `% len` ring indexing with `& (LEN - 1)` only when capacity is a
  power-of-two constant. This removes a per-sample division; never apply the mask
  to arbitrary lengths (manual - the modulo detector was dropped as unsound).

```rust
// bad
self.position = (self.position + 1) % length;
// good
self.position = (self.position + 1) & (LEN - 1);
```

- Break serial floating-point addition chains in whole-window RMS and energy work
  with `f32::algebraic_add`, or fold `chunks_exact(8)` into lane accumulators. Do
  not change RT callback math or bit-exact `f64` statistics (manual - the proposed
  float-sum detector was dropped as high-FP).
- Use `mul_add` only where scalar FMA exists. Keep it on Apple aarch64; on wasm32
  and baseline x86 use `a * b + c` unless FMA is guaranteed, then verify wasm emits
  no `fma` call (surfaced by the opt-in `cargo xtask audit-clippy` sweep).
- Never use `*_fast` float intrinsics or fast-float crates: NaN or infinity can
  produce poison in real audio. Use the `algebraic_*` family for reassociation and
  FMA freedom without value-based undefined behavior (manual - no lint encodes this).
- Keep DSP buffers planar per channel, interleave only at the device boundary, and
  use `u32` indices in hot structs while cold `String` and `PathBuf` metadata stays
  in side tables. kithara-stretch (bungee backend) and kithara-resampler already
  follow this layout (manual - no lint encodes this).

## Real-time callback and locking

- Cross the RT boundary with wait-free SPSC rings and atomic or ArcSwap snapshots.
  Never await, lock, allocate, trace, or release the last heap owner in the callback;
  preallocate at `prepare()` or `new_stream()` and defer drops off-thread (blocking
  subset caught by `arch.no-raw-no-block` plus the rtsan lane; allocation guarded by
  `assert_no_alloc`; raw channels banned by `arch.no-raw-tokio-sync`).
- Clone an `Arc` once per track, pass `&T` downward, and use `ArcSwap::load()` rather
  than `load_full()` per block. A guard avoids the atomic refcount bump; pre-warm
  ArcSwap debt and defer the last drop off-RT (manual - no lint encodes this).
- Use ArcSwap for hot read-mostly state instead of `RwLock::read()` per block;
  writers store a new `Arc`, with a small write-side mutex only when updates must
  serialize. kithara-hls/variant/map/offsets.rs migrated its hot byte map this way
  (manual - no lint encodes this).
- Suppress denormals in IIR and feedback state by freezing or zeroing state on
  silence, or by scoping FTZ and DAZ at RT-thread startup. Measure each target;
  Apple aarch64 handles subnormals better and wasm32 has no FTZ mechanism (manual -
  no lint encodes this).

## Async and observability

- Default to bounded channels sized to the buffering target so producers receive
  backpressure; use watch for latest-value state and fixed SPSC rings at the RT
  edge. Reserve unbounded channels for provably rate-limited control-plane producers
  and justify them locally (manual - the proposed detector was dropped as broken).
- Keep observability off hot paths: do not compute or allocate tracing fields per
  chunk. Guard expensive fields with `tracing::enabled!`, publish typed `Copy`
  events through the bounded deferred EventBus, and coalesce frequent events
  (manual - no lint encodes this outside the RT rules).

## Dispatch, build, and profiling

- Dispatch trait objects once per block or track, never once per sample, so the
  concrete inner kernel can inline and vectorize. Kithara's `Decoder`,
  `AudioEffect`, and `StretchBackend` seams already dispatch at coarse granularity;
  use enums for closed sets (manual - the dyn detector was dropped as high-FP).
- Build hot DSP crates at `opt-level = 3`, not workspace size optimization; cover
  kithara-resampler, kithara-decode, kithara-stretch, kithara-audio, and kithara-beat.
  Pair wasm with `+simd128` and inspect kernels with `cargo-show-asm` (manual -
  checked by Cargo.toml census).
- Benchmark the shipping codegen rather than Cargo's unrelated bench defaults.
  Match LTO, codegen units, panic, and disabled debug assertions; keep a profiling
  profile with symbols, and pass benchmark inputs to `black_box` by reference
  (manual - Cargo.toml census and bench-writing discipline).
- Keep UniFFI and wasm-bindgen surfaces coarse: expose opaque `Arc` handles, scalar
  getters, batched snapshots, and observers rather than per-frame `String`, `Vec`,
  record, enum, or JSON marshalling. kithara-ffi already follows this shape on both
  surfaces (manual - no lint encodes this).
