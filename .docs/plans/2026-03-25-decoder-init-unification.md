# Decoder Init Unification

**Status**: planned
**Date**: 2026-03-25

## Problem

Decoder creation happens in two separate places with diverging logic:

1. **Initial creation** (`audio.rs:664-676`): `spawn_blocking` → `DecoderFactory::create_from_media_info` / `create_with_probe`. No OffsetReader, no base_offset, no emit callback during creation.

2. **Recreation** (`pipeline/source.rs:405-429`): `(self.decoder_factory)(stream, info, base_offset)` → factory closure in `audio.rs:711-743`. Uses OffsetReader with base_offset, has byte_len computation, different error handling.

The factory closure (`audio.rs:711`) duplicates concerns: byte_len computation, OffsetReader wrapping, epoch tracking. Each bug fix (e.g. byte_len recompute, base_offset=0 enforcement) must account for both paths.

## Goal

Single `DecoderInit` entry point that handles:
- OffsetReader wrapping (base_offset translation)
- byte_len computation and update
- DecoderReady event emission
- Error handling and fallback (create_from_media_info → probe)

Both initial creation and recreation call the same path.

## Constraints

- Initial creation runs in spawn_blocking (no emit callback available inside)
- Recreation runs on the worker thread (synchronous)
- OffsetReader is only needed when base_offset > 0
- byte_len_handle is shared via Arc<AtomicU64>

## Sketch

```
struct DecoderInit {
    factory: DecoderFactory,      // codec-level creation
    byte_len_handle: Arc<AtomicU64>,
    stream_ctx: Arc<dyn StreamContext>,
    epoch: Arc<AtomicU64>,
}

impl DecoderInit {
    fn create(&self, stream, info, base_offset) -> Result<DecoderSession> {
        // 1. Compute byte_len
        // 2. Wrap in OffsetReader if base_offset > 0
        // 3. Create decoder via factory
        // 4. Update byte_len
        // 5. Return session (decoder + base_offset + media_info)
    }
}
```

Initial creation: `DecoderInit::create(stream, info, 0)`
Recreation: `DecoderInit::create(stream, new_info, base_offset)`
Event emission stays outside (caller emits DecoderReady after success).

## Non-goals

- Changing Symphonia/decoder internals
- Changing the OffsetReader coordinate translation
- Changing the ABR/format-change detection logic
