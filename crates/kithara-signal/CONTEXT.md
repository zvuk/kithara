# kithara-signal - Context

## Ownership

This crate is the canonical decoded-audio signal data plane. It owns
`AudioSpec`, `AudioChunkInfo`, `AudioChunk`, `FrameCount`, `SampleCount`, sample
sanitation, and pure frame/sample/duration conversion.

`AudioChunk` owns a `kithara-bufpool::SampleBuffer`; `kithara-bufpool` remains
the owner of allocation budgets, pool-region mechanics, recycling, and
`SampleBuffer`. `kithara-stream` remains the owner of encoded/container media
facts and transport positions.

## Dependency Boundary

The crate may depend downward on `kithara-platform` primitives and
`kithara-bufpool`. It must not depend on decode, stream, network, assets,
workers, schedulers, Warp, stretch, playback, Host, analyzer code, or backend
features.

Decoder, playback, Warp, and analyzer crates consume these values without
creating aliases or duplicate decoded-signal types.

`segment_index`, `variant_index`, and `render_revision` are opaque provenance
values supplied by producers; this crate stores but does not interpret them or
own decoder, protocol, or Warp policy. Keeping the revision with the samples
preserves causality through buffering signal processors.

## Stable Value Shapes

`AudioSpec`, `AudioChunkInfo`, and `AudioChunk` intentionally remain directly
constructible named-field values. Existing workspace crates build fixtures and
read hot-path fields through struct literals and direct access; this mechanical
extraction preserves that stable value contract instead of introducing a
parallel builder migration.

## Runtime Contract

Construction and resize may reserve storage only through the caller-injected
`PoolRegion<S>` where `S: HasPool<f32>`; borrowed views and layout conversions do not allocate. These
types do not start work or own mutable runtime coordination. Moving them into
this crate must preserve buffer lifetime, timeline fields, ordering, and
failure behavior exactly.
