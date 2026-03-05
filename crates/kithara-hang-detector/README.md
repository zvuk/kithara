# kithara-hang-detector

## Purpose
Runtime watchdog primitives for detecting stuck loops and invalid thread usage patterns.

## Owns
- `HangDetector` deadline/tick/reset logic
- `hang_watchdog!` helper macro
- Shared guard patterns used by platform/runtime crates

## Integrates with
- Re-exported by `kithara-platform`

## Notes
- Intended for defensive runtime checks, not business logic.
