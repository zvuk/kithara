# Adaptive Bitrate

How Kithara chooses HLS quality, and when to override it with ``AbrMode``.

## What ABR is

An HLS stream (`master.m3u8`) advertises several **variants** — the same
audio encoded at different bitrates (e.g. 64 / 128 / 256 kbps). *Adaptive
Bitrate* (ABR) is the engine continuously picking which variant to download
next, trading audio quality against the measured network throughput and the
amount already buffered, so playback keeps up without stalling.

Each variant is described by a ``Variant`` (its index in the ladder and its
advertised bandwidth). The ladder is published once per item via
`KitharaPlayerItem.variantsDiscovered`, and the variant currently being
applied via `variantApplied` / `variantSelected`.

## ``AbrMode``

``AbrMode`` selects *who decides* the variant:

- ``AbrMode/auto`` — the engine decides automatically from the throughput
  estimate and buffer level. This is the default and the right choice for
  almost all apps: it up-switches when bandwidth allows and down-switches
  (or escapes a stalled variant) before the buffer drains.

- ``AbrMode/manual(variantIndex:)`` — pin playback to one specific variant
  by its index in the ladder. ABR stops adapting; the engine fetches only
  that variant until you switch back to `.auto` or another index. Use this
  for a user-facing "quality" picker or for testing a specific rendition.

```swift
// Let the engine adapt (default):
player.setAbrMode(.auto)

// Pin to the highest variant the item exposes:
if let top = item.variants.indices.last {
    player.setAbrMode(.manual(variantIndex: top))
}
```

`variantIndex` is the zero-based position in the variant ladder reported by
`variantsDiscovered`. Indices that don't exist are ignored by the engine.

## Notes

- ``AbrMode`` only affects multi-variant HLS. Single-variant HLS and
  progressive files have nothing to adapt, so the mode is a no-op there.
- Switching mode is safe at any time, including during playback; the engine
  applies it at the next segment boundary so audio stays continuous.
- A manual pin does not disable buffering or stall recovery — it only fixes
  *which* variant is fetched.

## Topics

- ``AbrMode``
- ``Variant``
- ``KitharaPlayer/setAbrMode(_:)``
