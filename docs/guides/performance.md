# Performance

Lazy-loaded performance-antipattern reference, split by theme. Open only the sub-file that matches your task. Every entry is bad -> good with the detector that catches it (enforced ast-grep/clippy lint, the opt-in `cargo xtask audit-clippy` sweep, or manual).

*Tiers: hot = audio/decode/resampler/stretch/beat/bufpool; warm = stream/hls/file/net/storage/assets/abr/drm/queue/events/play; cold = platform/app/apple/ffi/encode.*

- [Allocation & memory](performance/allocation.md) - buffers, pools, clones, capacity, arenas.
- [API, iterators & collections](performance/api-iterators.md) - signatures & borrows, iterator fusion, data-structure choice.
- [Real-time callback & locking](performance/realtime.md) - RT-thread discipline, lock-free, atomics & ordering.
- [Async & concurrency](performance/async.md) - channels, backpressure, tokio, spawn/blocking discipline.
- [DSP math & data layout](performance/dsp-layout.md) - float/FMA/denormals, SIMD, cache & planar layout.
- [Dispatch & build](performance/dispatch-build.md) - dyn vs generics, inline, LTO/opt-level/codegen.
- [Strings, logging, IO & FFI](performance/io-ffi.md) - allocation-free logging, coarse FFI/wasm surface.
- [Benchmarking & methodology](performance/benchmarking.md) - measuring the shipping codegen, xruns, repo invariants, backlog.
