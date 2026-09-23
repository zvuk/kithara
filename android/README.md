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

## Demo App

[`example`](example) is a minimal player; `just platform android run` launches
it.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-android) for
the detailed contract.

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or
[MIT license](../LICENSE-MIT) at your option.
