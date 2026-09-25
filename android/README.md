<div align="center">

<img src="../logo.svg" alt="kithara" width="300">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Kithara for Android

Kotlin bindings for the Kithara audio engine: queue-based playback with seek,
adaptive bitrate, and reactive state through `StateFlow`. Ships as
`kithara.aar` with JNI libraries for `arm64-v8a` and `x86_64`, plus
`kithara-okhttp.aar`, the HTTP transport over OkHttp.

## Build

```bash
just platform android                          # JNI libraries + Kotlin bindings, debug
just platform android aar                      # release AARs
just platform android run                      # install and launch the demo
just platform android test                     # tests on an emulator
```

Run the build once before the first IDE sync: the generated Kotlin bindings are
a source directory of the `lib` module.

## Installation

An AAR carries no dependency metadata, so the application declares them:

```kotlin
dependencies {
    implementation(files("libs/kithara.aar"))
    implementation("net.java.dev.jna:jna:5.18.1@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")

    implementation(files("libs/kithara-okhttp.aar"))
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
}
```

The last two lines are needed only with the OkHttp transport.

## Quick Start

```kotlin
// Application.onCreate
Kithara.initialize(applicationContext, OkHttpTransport(okHttpClient))

val player = KitharaPlayer()
val item = KitharaPlayerItem("https://example.com/track.mp3")

lifecycleScope.launch {
    player.insert(item)
    player.play()
}
```

Every request runs through the application's HTTP client, which owns TLS,
proxies, cookies, timeouts and pooling. An application on another client
implements `com.kithara.net.HttpTransport`, whose documentation states the
protocol. The process keeps the first transport: initializing again with
another one throws.

## Usage

```kotlin
player.pause()
player.playingRate = 1.5f
player.seek(30.0)

lifecycleScope.launch { player.state.collect { println("${it.status} ${it.currentTime}s") } }

val hls = KitharaPlayerItem(
    url = "https://example.com/stream.m3u8",
    preferredPeakBitrate = 256_000.0,
    additionalHeaders = mapOf("Authorization" to "Bearer <token>"),
)

val store = AssetStore(root = application.filesDir.resolve("kithara-cache").absolutePath)
val cached = KitharaPlayer(config = KitharaPlayer.Config(store = store))
```

### Cache location and layout

`Kithara.initialize` creates one process-wide `AssetStore` rooted at
`<application cacheDir>/kithara`, shared by every default-configured player. A
different root or custom path layout means constructing another store:

```kotlin
val layouts = AssetLayoutRegistry().apply {
    register(MyFileAssetLayout(), AssetLayoutTarget.File)
    register(MyHlsAssetLayout(), AssetLayoutTarget.Hls)
}
val store = AssetStore(
    root = application.filesDir.resolve("kithara-cache").absolutePath,
    layouts = layouts,
)
val player = KitharaPlayer(config = KitharaPlayer.Config(store = store))
```

Ownership: `AssetLayoutRegistry` is the native Rust registry, so `register`
routes the layout into Rust immediately and Kotlin keeps no second copy. A store
captures a registry snapshot at construction — later registrations reach only
later stores — and one store can then be shared by any number of players. An
empty registry uses Kithara's defaults. `MyFileAssetLayout` and
`MyHlsAssetLayout` implement `AssetLayout`; their `root(source)` and
`path(resource)` callbacks choose paths below the outer cache directory, and
invalid callback output is rejected rather than rewritten or replaced with a
default. The `AssetLayout` API contract owns the portable component rules.

For signed media URLs whose path is shared by several tracks or variants, use
the built-in query-identity layout, registered once per protocol that serves
those URLs:

```kotlin
val queryIdentity = AssetLayouts.queryIdentity(
    rules = listOf(
        CacheIdentityRule(
            domains = listOf("media.example.com", "*.cdn.example.com"),
            queryParameters = listOf("track_id", "variant"),
        ),
    ),
)
```

Rules are checked in order. Domain patterns are exact hosts, `*.example.com`
for subdomains only, or `*` for every host. Only the named parameters
contribute to cache identity, so rotating signatures and expiry timestamps do
not split the cache; selected values are hashed into safe path components and
the raw query is never written to disk.

## Architecture

| Layer | Contract |
|-------|----------|
| `com.kithara` | Public Kotlin API, `StateFlow`-based reactive state |
| `com.kithara.ffi` | Generated UniFFI types and low-level bindings, including host configuration |
| `libkithara_ffi.so` | Rust core (kithara-play, kithara-ffi) |

The release AAR decodes the AAC family, MP3, and FLAC through the Android
`MediaCodec` backend over `MediaExtractor`.

## Demo App

[`example`](example) is a minimal player; `just platform android run` launches
it.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-android) for
the detailed contract.

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or
[MIT license](../LICENSE-MIT) at your option.
