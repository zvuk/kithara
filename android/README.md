<div align="center">
  <img src="../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![Kotlin](https://img.shields.io/badge/Kotlin-2.0-purple.svg)](https://kotlinlang.org)
[![Platforms](https://img.shields.io/badge/Android-API%2026%2B-green.svg)]()
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Kithara for Android

Android library providing Kithara audio engine bindings. Queue-based playback API with seek, adaptive bitrate support, and reactive state via Kotlin `StateFlow`.

Built on top of the Rust core via UniFFI-generated bindings, distributed as a Gradle module with JNI libraries for `arm64-v8a`, `armeabi-v7a`, and `x86_64`.

## Installation

Clone the repository and open the `android/` directory as a Gradle project in Android Studio.

To use Kithara as an AAR in your own project, build the release artifact (see [Building the AAR](#building-the-aar)) and add it as a file dependency alongside its transitive dependencies (`jna`, `kotlinx-coroutines-core`, `rustls-platform-verifier`).

Before the first Android Studio sync, build the JNI libraries and Kotlin UniFFI bindings (see [Development](#development)).

## Development

**Prerequisites:**

- Rust toolchain via [rustup](https://rustup.rs)
- `cargo-ndk`: `cargo install cargo-ndk`
- Android NDK installed and `ANDROID_NDK_HOME` set
- Rust targets for Android:

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
```

**Build JNI libraries and generate Kotlin UniFFI bindings:**

```bash
just android                              # debug (default)
cargo xtask android                       # equivalent
cargo xtask android --profile release     # release (optimized)
```

Output:
- `android/lib/build/generated/jniLibs/` — native `.so` libraries per ABI
- `android/lib/build/generated/uniffi/kotlin/` — generated Kotlin bindings

Run the build once before opening the project in Android Studio.

## Quick Start

Initialize the engine in your `Application` class, then create and play items:

```kotlin
// Application.onCreate:
Kithara.initialize(applicationContext)

// In any coroutine scope:
val player = KitharaPlayer()
val item = KitharaPlayerItem("https://example.com/track.mp3")

lifecycleScope.launch {
    item.load()
    player.insert(item)
    player.play()
}
```

## Usage

### Playback Control

```kotlin
player.play()
player.pause()
player.defaultRate = 1.5f   // playback speed
```

### Seek

```kotlin
try {
    player.seek(30.0)
} catch (e: KitharaError) {
    // handle seek failure
}
```

### Queue Management

```kotlin
val first = KitharaPlayerItem("https://example.com/a.mp3")
val second = KitharaPlayerItem("https://example.com/b.mp3")

lifecycleScope.launch {
    first.load()
    second.load()
    player.insert(first)
    player.insert(second, after = first)
    player.remove(first)
    player.removeAllItems()
}
```

### Player State (StateFlow)

```kotlin
lifecycleScope.launch {
    player.state.collect { state ->
        println("Status: ${state.status}")
        println("Position: ${state.currentTime}s")
        println("Duration: ${state.duration}s")
        println("Rate: ${state.rate}")
        state.error?.let { println("Error: $it") }
    }
}

// Current item changes
lifecycleScope.launch {
    player.currentItemChanges.collect {
        println("Current item changed")
    }
}
```

### Item State (StateFlow)

```kotlin
val item = KitharaPlayerItem("https://example.com/track.mp3")

lifecycleScope.launch {
    item.state.collect { state ->
        println("Item status: ${state.status}")
        state.error?.let { println("Load failed: $it") }
    }
}

lifecycleScope.launch {
    item.load()
}
```

### ABR Bitrate Hints

```kotlin
item.preferredPeakBitrate = 256_000.0                  // cap quality
item.preferredPeakBitrateForExpensiveNetworks = 128_000.0  // lower cap on metered networks
```

### Additional HTTP Headers

```kotlin
val item = KitharaPlayerItem(
    url = "https://example.com/protected.mp3",
    additionalHeaders = mapOf("Authorization" to "Bearer <token>"),
)
```

## Architecture

```
┌─────────────────────────────────────────┐
│  com.kithara (Kotlin API)               │
│  KitharaPlayer, KitharaPlayerItem       │
├─────────────────────────────────────────┤
│  com.kithara.ffi (UniFFI-generated)     │
├─────────────────────────────────────────┤
│  libkithara_ffi.so                      │
│  (Rust core: kithara-play, kithara-ffi) │
└─────────────────────────────────────────┘
```

| Layer | Description |
|-------|-------------|
| **com.kithara** | Public Kotlin API with `StateFlow`-based reactive state |
| **com.kithara.ffi** | Auto-generated UniFFI bindings — not intended for direct use |
| **libkithara_ffi.so** | Native shared library built from the Rust crate |

## Demo App

A minimal Android demo player is included in [`example`](example). Plays audio from a URL or a local file picked from device storage, with play/pause, stop controls, and reactive status display.

Open the `android/` directory as a Gradle project in Android Studio and run the `:example` configuration, or build and install via the command line:

```bash
cd android
./gradlew :example:installDebug
```

## Building the AAR

Builds the Rust core for all supported ABIs and packages it into a release AAR:

```bash
just android-aar
```

Output: `android/lib/build/outputs/aar/lib-release.aar` with JNI slices for:
- `arm64-v8a`
- `armeabi-v7a`
- `x86_64`

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or [MIT license](../LICENSE-MIT) at your option.
