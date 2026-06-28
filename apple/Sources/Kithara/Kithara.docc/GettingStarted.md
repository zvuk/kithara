# Getting Started

Create a player, queue an item, and observe playback.

## Add the framework

Kithara ships as a binary `XCFramework` plus the `Kithara` Swift layer.
Add it via Swift Package Manager (the `Kithara` product) or by dropping
the `XCFramework` into your target and importing the module:

```swift
import Kithara
```

## Create a player

``KitharaPlayer`` owns the engine and the playback queue. Construct it
with a ``KitharaPlayer/Config`` (defaults are fine for plain playback):

```swift
let player = KitharaPlayer()
```

For caching or DRM, configure ``KitharaPlayer/Config``:

```swift
var config = KitharaPlayer.Config()
config.cacheDir = NSTemporaryDirectory() + "kithara-cache"
let player = KitharaPlayer(config: config)
```

## Queue an item and play

A ``KitharaPlayerItem`` wraps a source — a remote HLS playlist
(`master.m3u8`), a progressive media URL, or a local file:

```swift
let item = try KitharaPlayerItem(url: url)
try player.append(item)
player.play()
```

Use ``KitharaPlayer/insert(_:after:)`` and ``KitharaPlayer/remove(_:)`` to
manage the queue, and ``KitharaPlayer/items()`` to read it.

## Control playback

```swift
player.pause()
player.play()
player.seek(to: 42.0)                 // seconds
player.volume = 0.8
```

## Observe state with Combine

Playback state is published, not polled. Subscribe on the main actor:

```swift
let bag = Set<AnyCancellable>()

player.currentTimePublisher
    .sink { time in slider.value = Float(time) }
    .store(in: &bag)

player.eventPublisher
    .sink { event in print("player event: \(event)") }
    .store(in: &bag)

player.error
    .sink { error in show(error) }
    .store(in: &bag)
```

`snapshot` gives a synchronous one-shot read (status, time, duration,
rate, volume) when you need current values without subscribing.

## Next

- Tune quality selection in <doc:AdaptiveBitrate>.
- Browse the full API on ``KitharaPlayer`` and ``KitharaPlayerItem``.
