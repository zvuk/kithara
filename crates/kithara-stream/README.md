# kithara-stream

## Purpose
Async producer to sync consumer bridge for media streaming.

## Owns
- `Downloader` orchestration backend
- Sync `Read + Seek` stream interface
- Canonical shared media types (`AudioCodec`, `ContainerFormat`, `MediaInfo`)

## Integrates with
- Protocol downloaders (`kithara-file`, `kithara-hls`)
- Decoder/pipeline layers (`kithara-decode`, `kithara-audio`)

## Notes
- Keep shared media type definitions centralized here.
