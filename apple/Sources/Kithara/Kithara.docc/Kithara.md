# ``Kithara``

A streaming audio player for Apple platforms — adaptive HLS, progressive
files, gapless playback, an equalizer, and DRM — backed by the Kithara
engine (Rust core delivered as an `XCFramework`).

## Overview

`Kithara` exposes one main entry point, ``KitharaPlayer``, and a queue of
``KitharaPlayerItem`` values. You create a player, add items (remote HLS or
progressive URLs, or local files), and drive playback with `play()`,
`pause()`, and `seek(to:)`. State and lifecycle are observed through Combine
publishers (`eventPublisher`, `currentTimePublisher`, `error`, …) so the
player integrates directly into SwiftUI / UIKit view models.

The audio engine handles segment fetching, decoding (AAC, FLAC, MP3, …),
adaptive bitrate switching, and an output graph (equalizer, volume) on its
own threads; the public API is `@MainActor` where it touches UI-facing
state and returns values immediately.

```swift
import Kithara

let player = KitharaPlayer()
let item = try KitharaPlayerItem(url: URL(string: "https://example.com/master.m3u8")!)
try player.append(item)
player.play()
```

## Topics

### Essentials

- <doc:GettingStarted>
- ``KitharaPlayer``
- ``KitharaPlayerItem``

### Adaptive Bitrate (HLS)

- <doc:AdaptiveBitrate>
- ``AbrMode``
- ``Variant``

### Playback Control

- ``KitharaPlayer/play()``
- ``KitharaPlayer/pause()``
- ``KitharaPlayer/seek(to:completion:)``
- ``KitharaPlayer/setAbrMode(_:)``

### Queue

- ``KitharaPlayer/append(_:)``
- ``KitharaPlayer/insert(_:after:)``
- ``KitharaPlayer/remove(_:)``
- ``KitharaPlayer/items()``

### Observing State

- ``KitharaPlayer/eventPublisher``
- ``KitharaPlayer/currentTimePublisher``
- ``KitharaPlayer/error``
- ``KitharaPlayer/snapshot``
- ``PlayerStatus``

### Output Graph

- ``KitharaPlayer/setEqGain(band:gainDb:)``
- ``KitharaPlayer/eqGain(band:)``
- ``KitharaPlayer/resetEq()``
- ``KitharaPlayer/volume``
- ``KitharaPlayer/isMuted``

### DRM

- ``KitharaPlayer/KeyRule``
- ``KitharaPlayer/Config``
