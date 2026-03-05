# kithara

## Purpose
Facade crate that auto-detects source type (progressive file or HLS) and exposes a unified `Resource` API.

## Owns
- Source auto-detection and facade wiring
- Shared public entrypoint for consumers
- Feature-gated protocol composition (`file`, `hls`)

## Integrates with
- `kithara-file`, `kithara-hls`
- `kithara-stream`, `kithara-play`

## Notes
- Use this crate when you need one API across protocols.
- Prefer lower-level crates only when you need protocol-specific control.
