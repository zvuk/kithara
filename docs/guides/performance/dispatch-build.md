# Dispatch & build

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

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

**`Box<dyn T>` return where `impl Trait` fits** - watch for boxing a single concrete return type; absent here - every `Box<dyn>` return (`build_backend`, `create_decoder`) is a runtime-selected, *stored* trait object that `impl Trait` cannot express. *tier: hot | detector: opt-in `just lint audit-clippy` (`unnecessary_box_returns`, pedantic) | already clean*

## Build/codegen/LTO

**Size profile starves DSP autovectorization** (PARTLY FIXED) - the workspace ships `opt-level = "z"`, which disables loop autovectorization and lowers inline thresholds; with no SIMD crate, autovectorization is the *only* vector path, so the DSP that stays at `"z"` ships scalar.

```toml
# the override the workspace now carries
[profile.release.package.signalsmith-stretch]
opt-level = 3
```

A per-package override reaches two units, not one: the crate's own codegen, and its build script's `OPT_LEVEL` environment variable. `signalsmith-stretch` compiles its C++ through `cc`, which passes `OPT_LEVEL` straight to the compiler, so the override moves that C++ from `-Oz` to `-O3` as well. Verify kernels vectorize with `cargo-show-asm`. Cargo caveat: a per-package override sets `opt-level` but *not* `lto`/`panic`.
*tier: hot | detector: manual (Cargo.toml census) | `signalsmith-stretch` is at 3; the pure-Rust DSP (`rubato`, `rustfft`, `symphonia*`, `kithara-audio`/`-decode`/`-resampler`) is still at `"z"` and reaches the wasm bundle, where `web-size` enforces a budget - raising it is a measured change of its own*

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

**Size-lane `build-std-features`** - iOS/wasm rebuild std via the `[unstable]` sections in `.cargo/config.toml` / `crates/kithara-ffi/.cargo/config.toml` with `build-std-features = ["optimize_for_size"]` (no default std features, so std's backtrace machinery is out). `panic_immediate_abort` is no longer a std feature; it is the `-Cpanic=immediate-abort` strategy, applied per lane (wasm target rustflags; apple release RUSTFLAGS in `xtask apple`). *tier: cold | detector: manual | already-enforced*

**Cross-crate calls without LTO** - watch for hot cross-crate boundaries not being whole-program-optimized. Release is `lto="fat"` (wasm-release too), but the profile setting alone does not mean LTO ran: cargo skips LTO for any unit that also emits an rlib, so `kithara-ffi` (`crate-type = ["lib", "staticlib", "cdylib"]`) linked un-optimized until `xtask apple` began relinking each slice as a `staticlib`-only unit. Read the artifact, not the profile - `ar t` on a fat-LTO archive lists a handful of objects, not one per crate. *tier: hot | detector: `ar t` on the slice | already-enforced*

**Nightly `std::simd` / dead SIMD crates** - watch for `feature(portable_simd)`, `faster`, or `packed_simd`; absent. If explicit SIMD is ever added, use `wide` on stable (NEON + simd128 + SSE/AVX matches kithara's target matrix), not nightly `std::simd`. *tier: hot | detector: manual | preventive*

**Unexamined FFI panic strategy** - watch for default unwind crossing the uniffi/wasm surface; already decided: release `panic="abort"` + apple `panic_immediate_abort` + `unwrap_used=deny`. Do not rely on uniffi `catch_unwind` under abort. Covers O6 + O11. *tier: cold | detector: manual | already-enforced*

**Untuned release codegen** - watch for a shipped default profile; non-issue, `[profile.release]` is fully tuned (fat-LTO, cgu=1, `panic="abort"`, `strip="symbols"`, opt-z) - if anything too aggressive on the size axis (see the opt-z item). *tier: n/a | detector: manual | already-enforced*

**Unstripped mobile symbols** - watch for shipping symbols in the artifact; non-issue, release `strip="symbols"` + `debug=false` and apple strips slices. Keep archiving dSYM separately for crash symbolication. *tier: n/a | detector: manual | already-enforced*

**PGO/BOLT as a magic last mile** - watch for reaching for profile-guided opt before source fixes plateau; absent. If pursued, feed `cargo-pgo` real HLS/decode workloads (not unit tests); BOLT is ELF-only, so off the table for Mach-O iOS/macOS. *tier: n/a | detector: manual | preventive*
