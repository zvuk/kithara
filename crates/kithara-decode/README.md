# kithara-decode

## Purpose
Synchronous audio decoding with pluggable backend selection.

## Owns
- Decoder trait contracts
- Backend adapters (for example Symphonia)
- Conversion to workspace PCM types

## Integrates with
- `kithara-audio` pipeline
- `kithara-stream` read path

## Notes
- Decoding is sync by design; scheduling is handled by higher layers.
