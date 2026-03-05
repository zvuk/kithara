# kithara-events

## Purpose
Typed event bus used across playback components.

## Owns
- Event definitions and routing primitives
- Subscription/broadcast contracts
- Event boundary between engine and UI/application layers

## Integrates with
- `kithara-play`, `kithara-audio`, app/UI crates

## Notes
- Treat event payloads as contract surfaces; avoid leaking internals.
