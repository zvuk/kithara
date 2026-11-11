<div align="center">

<img src="../logo.svg" alt="kithara" width="300">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Kithara for Apple

Swift package for iOS and macOS: an AVPlayer-style API over the Kithara audio
engine with queue playback, volume and mute, seek, and adaptive bitrate. The
Rust core is exposed through UniFFI and ships as a pre-built XCFramework.

## Installation

The package manifest is the repository root `Package.swift`, not this
directory; it declares the minimum platforms and three products: `Kithara` (the
Swift API), `KitharaFFI` (generated UniFFI bindings, not for direct use), and
`KitharaRx` (an optional RxSwift bridge over the Combine publishers). They sit
on the `KitharaFFIInternal` binary target, the XCFramework holding the Rust
core. Depend on `https://github.com/zvuk/kithara` at a tag from the
[Releases page](https://github.com/zvuk/kithara/releases), or add the unpacked
`Kithara.xcframework.zip` from the release assets to the app target by hand.

**Binary selection trap.** `KITHARA_LOCAL_DEV` only *overrides* the choice;
with it unset the manifest uses a local `apple/KitharaFFIInternal.xcframework`
whenever that path exists, and the pinned release download otherwise. A stale
locally built framework therefore silently wins over the tagged binary — set
the variable to `0` to force the release download, or delete the local one.

## Build

```bash
just platform apple xcframework                    # release
just platform apple xcframework --profile debug
just platform apple demo                           # build and launch the demo
just platform apple xcode                          # generate and open the demo project
just platform apple ios                            # xcodebuild the iOS demo scheme
just platform apple test                           # iOS unit tests
just platform apple integration-regressions [name]
just platform apple doc                            # DocC for Kithara and KitharaRx
```

The XCFramework lands at `apple/KitharaFFIInternal.xcframework`. Release
carries `macos-arm64_x86_64`, `ios-arm64`, and the fat
`ios-arm64_x86_64-simulator`; **debug carries only `ios-arm64-simulator`**, so a
generic simulator destination asks for architectures the debug framework never
claimed and the link fails with a missing-symbol error for x86_64. Pin
`ARCHS=arm64` against a debug build.

Traps:

- `test` and `integration-regressions` need `KITHARA_TEST_SERVER_URL` pointing
  at a running hermetic test server, and resolve an iOS simulator themselves;
  KITHARA_IOS_DESTINATION overrides that choice with an xcodebuild destination.
- Explicit modules build the SDK's own PCMs into a shared module cache, and on
  a cold cache Xcode fails that step for the demo scheme with a redefinition of
  module `SwiftShims`. A warm cache hides it, so it reads as intermittent; the
  recipe disables explicit modules. Tool versions are pinned in
  `.config/ci-pins.toml`.

## Usage

### Playback and queue

```swift
import Kithara

let player = KitharaPlayer()
try player.insert(KitharaPlayerItem(url: "https://example.com/a.mp3"))
try player.insert(second, after: first)
player.remove(first); player.removeAllItems()

player.play(); player.pause()
player.stop()                    // pause + clear queue
player.advanceToNextItem()
player.volume = 0.5; player.isMuted = true
player.playingRate = 1.5         // target playback speed
player.seek(to: 30.0, tolerance: nil, completionHandler: MySeekCallback())

final class MySeekCallback: SeekCallback, @unchecked Sendable {
    func onComplete(finished: Bool) { print("Seek finished: \(finished)") }
}
```

### Network and DRM (HLS-AES)

Pass key rules, the auth token, and the crossfade window in `Config` so the
player starts in that state; `setupHlsAes`, `setupNetwork`, and
`crossfadeDuration` change them at run time.

```swift
final class SaltedCipher: KeyProcessor, @unchecked Sendable {
    let cipherKey: String

    init(cipherKey: String) { self.cipherKey = cipherKey }

    // The rule's `salt` is attached to every outgoing request under
    // `X-Encrypted-Key`. Build the cipher from the same salt so it matches
    // the server's encryption.
    func processKey(_ key: Data, salt: String) -> Data {
        Cipher(key: cipherKey + salt).decrypt(key)
    }
}

let player = KitharaPlayer(
    config: KitharaPlayer.Config(
        keyRules: [.wildcard(processor: SaltedCipher(cipherKey: cipherKey))],
        authToken: "<token>"
    )
)
player.updatePeakBitrate(wifi: 2_000_000, cellular: 500_000)
```

### Events

```swift
player.eventPublisher
    .receive(on: DispatchQueue.main)
    .sink { event in if case let .error(message) = event { print(message) } }
    .store(in: &cancellables)

item.eventPublisher.sink { /* same shape, per item */ }.store(in: &cancellables)

// Explicit preload is optional; insert can auto-load with player config.
Task { print("Playable: \(await item.load().isPlayable)") }
```

### Per-item options

```swift
let item = KitharaPlayerItem(url: hlsURL,
    preferredPeakBitrate: 256_000, preferredPeakBitrateForExpensiveNetworks: 0)
```

### Cache location and layout

```swift
let layouts = AssetLayoutRegistry()
layouts.register(MyFileAssetLayout(), for: .file)
layouts.register(MyHlsAssetLayout(), for: .hls)

let store = AssetStore(root: appSupportDirectory.path, layouts: layouts)
let player = KitharaPlayer(config: .init(store: store))
```

Ownership: `AssetStore` owns the outer cache directory and an immutable
snapshot of its layouts, so later registrations reach only later stores and one
store serves any number of players; the Rust registry takes a registration
immediately. An empty registry uses Kithara's defaults. `MyFileAssetLayout` and
`MyHlsAssetLayout` implement `AssetLayout`; their `root(source:)` and
`path(resource:)` callbacks are retained by Rust, choose paths below the outer
directory, and are rejected on invalid output rather than rewritten or
defaulted — the protocol documentation owns the portable component rules.

When remote URLs carry content identity in query parameters, register the
built-in domain-aware layout — through the same ordinary registration used for
application layouts — for each applicable protocol:

```swift
let queryIdentity = AssetLayouts.queryIdentity(rules: [
    CacheIdentityRule(
        domains: ["media.example", "*.cdn.example"],
        queryParameters: ["content_ref", "edition"]
    ),
    CacheIdentityRule(domains: ["*"], queryParameters: ["fallback_content_key"]),
])
layouts.register(queryIdentity, for: .file)
layouts.register(queryIdentity, for: .hls)
```

Rules are evaluated in order; domains are exact hosts, `*.domain` for
subdomains, or `*`. Only the listed parameters affect the cache root, so signed
URL parameters such as expiry timestamps are ignored, and the raw query is
never stored in a cache path.

## Demo and Playground

[`Examples/KitharaDemo`](Examples/KitharaDemo) plays any URL (MP3, AAC, FLAC,
HLS) with transport, seek, volume, and rate; launch it with
`just platform apple demo`. The
[playground](Examples/KitharaDemo/KitharaPlayground.playground) exercises the
core API without the app — build a debug XCFramework, then run
`KITHARA_LOCAL_DEV=1 open Package.swift` from the repository root.

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or
[MIT license](../LICENSE-MIT) at your option.
