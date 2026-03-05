# kithara-abr

## Purpose
Protocol-agnostic adaptive bitrate (ABR) logic used by streaming components.

## Owns
- Throughput/smoothing estimator
- Variant scoring and selection heuristics
- ABR decisions independent from HLS parser specifics

## Integrates with
- Primarily consumed by `kithara-hls`
- Can be reused by other adaptive protocols

## Notes
- Keeps ABR policy isolated from downloader implementation details.
