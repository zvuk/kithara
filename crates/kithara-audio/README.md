# kithara-audio

## Purpose
Audio pipeline that transforms decoded PCM into playback-ready output.

## Owns
- Audio worker lifecycle
- Effects chain (EQ and related transforms)
- Resampling and output chunk flow

## Integrates with
- `kithara-decode` for decoded frames
- `kithara-stream` as source bridge
- `kithara-play` as engine consumer

## Notes
- Hot paths use pooled buffers from `kithara-bufpool`.
