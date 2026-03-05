# kithara-drm

## Purpose
Segment decryption helpers for streaming workflows.

## Owns
- AES-128-CBC decryption path
- Decrypt context and chunk processors
- Extension points for key processing

## Integrates with
- `kithara-hls` fetch/decrypt flow
- `kithara-assets` cache write pipeline

## Notes
- Keep protocol-independent crypto logic here, not in HLS orchestration.
