# Zvuk Compatibility Overlay

This directory contains source overlay code for the Zvuk iOS `PlayerModule`.

Add these Swift files to the same target that declares `AudioPlayerProtocol`,
`AudioPlayerItemProtocol`, and `PlayerItemService`. Those protocols are
internal in Zvuk, so this overlay cannot be compiled as a separate Kithara
SwiftPM target.

The overlay keeps Kithara's public API Rx-free while adapting it to Zvuk's
legacy RxSwift protocols and `PlayerErrorMapper`.

`KitharaZvukAudioPlayerItem.load()` is intentionally a non-blocking
compatibility shim. Kithara opens and streams the source when the item is
inserted into `KitharaPlayer`, so old AVPlayer-style pre-insert item loading
must not become the playback startup gate.

Use `KitharaZvukPlayerItemService` instead of `PlayerItemServiceImpl` when the
audio player is `KitharaZvukAudioPlayer`. The old service builds
`AudioQueue.Item` / `AVURLAsset`; Kithara needs `KitharaZvukAudioPlayerItem`.

Zvuk code paths that expose `Observable<AudioQueue.Item>` are still AVPlayer
specific and should either become protocol-based or remain disabled for the
Kithara-backed player.
