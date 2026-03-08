<div align="center">
  <img src="../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![Swift 6.0](https://img.shields.io/badge/Swift-6.0-orange.svg)](https://swift.org)
[![Platforms](https://img.shields.io/badge/Platforms-iOS%2016%2B%20%7C%20macOS%2013%2B-blue.svg)]()
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Kithara for Apple

Swift package for iOS and macOS providing Kithara audio engine bindings. AVPlayer-style API with queue-based playback, volume/mute control, seek, and adaptive bitrate support.

Built on top of the Rust core via UniFFI-generated bindings and distributed as a Swift package with a pre-built XCFramework.

## Installation

Add Kithara as a Swift Package Manager dependency:

**Xcode**: File → Add Package Dependencies → enter `https://github.com/zvuk/kithara` → select "Up to Next Major Version" from `0.1.0`.

**Package.swift**:

```swift
// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "MyApp",
    platforms: [.iOS(.v16), .macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/zvuk/kithara", from: "0.1.0"),
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

## Development

For local development, clone the repo and use the `KITHARA_LOCAL_DEV` environment variable to build against the local XCFramework:

```bash
cargo xtask xcframework                    # build XCFramework (release)
cargo xtask xcframework --profile debug    # build XCFramework (debug)
KITHARA_LOCAL_DEV=1 swift build            # build Swift package with local binary
```

## Quick Start

```swift
import Kithara

let player = KitharaPlayer()
let item = KitharaPlayerItem(url: "https://example.com/track.mp3")
item.load()

try player.insert(item)
player.play()
```

## Usage

### Playback Control

```swift
player.play()
player.pause()
player.volume = 0.5
player.isMuted = true
player.defaultRate = 1.5   // playback speed
```

### Seek

```swift
player.seek(to: 30.0, callback: MySeekCallback())

final class MySeekCallback: SeekCallback, @unchecked Sendable {
    func onComplete(finished: Bool) {
        print("Seek finished: \(finished)")
    }
}
```

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

item.load()
```

### ABR Bitrate Hints

```swift
item.preferredPeakBitrate = 256_000               // cap quality
item.preferredPeakBitrateForExpensiveNetworks = 0  // unlimited on Wi-Fi
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

## Demo App

A minimal macOS demo player is included in [`Examples/KitharaDemo`](Examples/KitharaDemo). Plays audio from any URL (MP3, AAC, FLAC, HLS) with transport controls, seek, volume, playback rate, and error reporting.

```bash
just apple-demo
```

Or manually:

```bash
cargo xtask xcframework --profile debug
cd apple && swift run KitharaDemo
```

Features: URL input with Cmd+V, play/pause with auto-reload after track ends, seek slider, volume with mute, rate selector (0.5x–2.0x), status badge, and error display.

## Building the XCFramework

The XCFramework bundles the Rust core for all supported Apple platforms:

```bash
cargo xtask xcframework                    # release (optimized)
cargo xtask xcframework --profile debug    # debug (faster builds)
```

Output: `apple/KitharaFFIInternal.xcframework` with slices for:
- `macos-arm64_x86_64`
- `ios-arm64`
- `ios-arm64_x86_64-simulator`

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or [MIT license](../LICENSE-MIT) at your option.
