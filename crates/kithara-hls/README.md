# kithara-hls

## Purpose
HLS VOD orchestration with ABR, caching, and segment lifecycle management.

## Owns
- Playlist and segment coordination
- Variant selection integration with `kithara-abr`
- Decrypt/cache/fetch orchestration for HLS assets

## Integrates with
- `kithara-net`, `kithara-assets`, `kithara-stream`, `kithara-drm`

## Notes
- Keep protocol-specific orchestration here, not in facade/player crates.
