<div align="center">
  <img src="../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Swift 6.0](https://img.shields.io/badge/Swift-6.0-orange.svg)](https://swift.org)
[![Platforms](https://img.shields.io/badge/Platforms-iOS%2016%2B%20%7C%20macOS%2013%2B-blue.svg)]()
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Kithara for Apple

Swift package for iOS and macOS providing Kithara audio engine bindings. AVPlayer-style API with queue-based playback, volume/mute control, seek, and adaptive bitrate support.

Built on top of the Rust core via UniFFI-generated bindings and distributed as a Swift package with a pre-built XCFramework.

## Installation

Add Kithara as a Swift Package Manager dependency:

**Xcode**: File → Add Package Dependencies → enter `https://github.com/zvuk/kithara` → pick the latest tag from the [Releases page](https://github.com/zvuk/kithara/releases).

**Package.swift**:

```swift
// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "MyApp",
    platforms: [.iOS(.v16), .macOS(.v13)],
    dependencies: [
        // Replace X.Y.Z with the latest tag from https://github.com/zvuk/kithara/releases
        .package(url: "https://github.com/zvuk/kithara", from: "X.Y.Z"),
    ],
    targets: [
        .executableTarget(
            name: "MyApp",
            dependencies: [
                .product(name: "Kithara", package: "Kithara"),
            ]
        ),
    ]
)
```

For manual integration, download `Kithara.xcframework.zip` from the release
assets and add the unpacked `Kithara.xcframework` to the app target.

## Development

For local development, clone the repo and use the `KITHARA_LOCAL_DEV` environment variable to build against the local XCFramework:

```bash
cargo xtask apple build                    # build XCFramework (release)
cargo xtask apple build --profile debug    # build XCFramework (debug)
KITHARA_LOCAL_DEV=1 swift build            # build Swift package with local binary
```

## Quick Start

```swift
import Kithara

let player = KitharaPlayer()
let item = KitharaPlayerItem(url: "https://example.com/track.mp3")

try player.insert(item)
player.play()
```

## Usage

### Playback Control

```swift
player.play()
player.pause()
player.stop()                         // pause + clear queue
player.advanceToNextItem()
player.volume = 0.5
player.isMuted = true
player.playingRate = 1.5              // target playback speed
```

### Runtime DRM (HLS-AES)

```swift
player.setupHlsAes { encryptedKey, salt in
    // The player generates `salt` and attaches it to every outgoing
    // request under `X-Encrypted-Key`. Build the cipher from the
    // same salt to match the server's encryption.
    let cipher = Cipher(key: cipherKey + salt)
    return cipher.decrypt(encryptedKey)
}
```

### Network defaults

```swift
player.setupNetwork(authToken: "<token>")
player.updatePeakBitrate(wifi: 2_000_000, cellular: 500_000)
```

### Cache location and layout

`cacheDir` selects the outer directory for the whole asset store. Register
layouts by protocol when an application needs a different path contract below
each asset root:

```swift
var layouts = CacheLayoutRegistry()
layouts.register(MyFileCacheLayout(), for: .file)
layouts.register(MyHlsCacheLayout(), for: .hls)

let player = KitharaPlayer(
    config: .init(
        cacheDir: appSupportDirectory.path,
        layouts: layouts
    )
)
```

`MyFileCacheLayout` and `MyHlsCacheLayout` implement `CacheLayoutDelegate`.
Their `root(source:)` and `path(resource:)` callbacks are captured at player
creation. An empty registry uses Kithara's defaults. Invalid callback output is
rejected rather than rewritten or replaced with a default path; see the
protocol documentation for the portable component rules.

### Seek

```swift
player.seek(to: 30.0, tolerance: nil, completionHandler: MySeekCallback())

final class MySeekCallback: SeekCallback, @unchecked Sendable {
    func onComplete(finished: Bool) {
        print("Seek finished: \(finished)")
    }
}
```

### Migration from earlier API

| Old | New |
|-----|-----|
| `player.defaultRate` | `player.playingRate` |
| `player.seek(to:callback:)` | `player.seek(to:tolerance:completionHandler:)` |
| `player.setPreferredPeakBitrate(...)` | `player.updatePeakBitrate(wifi:cellular:)` |
| `KeyProcessor.processKey(_ key:)` | `KeyProcessor.processKey(_ key:salt:)` |
| content `KitharaPlayerItem.id` | `KitharaPlayerItem.audioId`; `id` is now the unique queue-item identity |
| `ItemEvent.bufferedDurationChanged(seconds:)` | `ItemEvent.loadedRangesChanged(ranges:)` |
| `currentTime: TimeInterval?` | `currentTime: TimeInterval` (`0` when no item is loaded) |

### Queue Management

```swift
let first = KitharaPlayerItem(url: "https://example.com/a.mp3")
let second = KitharaPlayerItem(url: "https://example.com/b.mp3")

try player.insert(first)
try player.insert(second, after: first)
player.remove(first)
player.removeAllItems()
```

### Events (Combine)

```swift
player.eventPublisher
    .receive(on: DispatchQueue.main)
    .sink { event in
        switch event {
        case let .timeChanged(seconds):
            print("Position: \(seconds)s")
        case let .rateChanged(rate):
            print("Rate: \(rate)")
        case let .statusChanged(status):
            print("Status: \(status)")
        case let .durationChanged(seconds):
            print("Duration: \(seconds)s")
        case let .error(message):
            print("Error: \(message)")
        case let .currentItemChanged(itemId):
            print("Now playing: \(itemId ?? "none")")
        default:
            break
        }
    }
    .store(in: &cancellables)
```

### Item Events

```swift
let item = KitharaPlayerItem(url: url)

item.eventPublisher
    .sink { event in
        if case let .error(message) = event {
            print("Load failed: \(message)")
        }
    }
    .store(in: &cancellables)

// Explicit preload is optional. Insert can auto-load with player config.
Task {
    let result = await item.load()
    print("Playable: \(result.isPlayable)")
}
```

### ABR Bitrate Hints

```swift
let item = KitharaPlayerItem(
    url: "https://example.com/stream.m3u8",
    preferredPeakBitrate: 256_000,
    preferredPeakBitrateForExpensiveNetworks: 0
)
```

## Architecture

```
┌─────────────────────────────────────────┐
│  Kithara (Swift API)                    │
│  KitharaPlayer, KitharaPlayerItem       │
├─────────────────────────────────────────┤
│  KitharaFFI (UniFFI-generated bindings) │
├─────────────────────────────────────────┤
│  KitharaFFIInternal.xcframework         │
│  (Rust core: kithara-play, kithara-ffi) │
└─────────────────────────────────────────┘
```

| Layer | Description |
|-------|-------------|
| **Kithara** | Swifty public API with Combine publishers and AVPlayer-style semantics |
| **KitharaFFI** | Auto-generated UniFFI bindings — not intended for direct use |
| **KitharaFFIInternal** | Pre-built static library (XCFramework) from the Rust crate |

## Interactive Playground

There is also an interactive Swift playground at [`Examples/KitharaDemo/KitharaPlayground.playground`](Examples/KitharaDemo/KitharaPlayground.playground).
It is useful for quickly trying core APIs (`load`, `insert`, `play`, `pause`, `seek`, `volume`, `mute`, and playback rate) without running the full demo app.

Open `apple/Package.swift` in Xcode, then open the playground from the Project navigator and run it.
For local Rust changes, build a local XCFramework first:

```bash
cargo xtask apple build --profile debug
cd apple
KITHARA_LOCAL_DEV=1 open Package.swift
```
## Demo App

An iOS/macOS demo player is included in [`Examples/KitharaDemo`](Examples/KitharaDemo). It plays audio from any URL (MP3, AAC, FLAC, HLS) with transport controls, seek, volume, playback rate, and error reporting.

```bash
cargo xtask apple run
just apple demo
```

To open the generated Xcode project instead of launching a simulator:

```bash
just apple xcode
```

Features: URL input with Cmd+V, play/pause with auto-reload after track ends, seek slider, volume with mute, rate selector (0.5x–2.0x), status badge, and error display.

## Building the XCFramework

The XCFramework bundles the Rust core for all supported Apple platforms:

```bash
just apple xcframework                       # release (optimized)
just apple xcframework --profile debug       # debug (faster builds)
# Equivalent direct xtask invocations:
cargo xtask apple build
cargo xtask apple build --profile debug
```

Output: `apple/KitharaFFIInternal.xcframework` with slices for:
- `macos-arm64_x86_64`
- `ios-arm64`
- `ios-arm64_x86_64-simulator`

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or [MIT license](../LICENSE-MIT) at your option.
