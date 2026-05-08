import Combine
import Foundation
import KitharaFFI

/// A single audio item that can be queued in ``KitharaPlayer``.
///
/// All preferences (bitrate caps, ABR mode, headers) are frozen at
/// construction. Loading starts automatically when the item is inserted
/// into a ``KitharaPlayer``.
///
/// ```swift
/// let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
/// try player.insert(item)
/// ```
public final class KitharaPlayerItem: Identifiable, @unchecked Sendable {
    /// Stable per-item identifier (UUIDv4 string). Mirrors the iOS
    /// `AudioPlayerItemProtocol.audioId` — synonym retained for
    /// `Identifiable` conformance.
    public nonisolated var id: String { audioId }

    /// Stable per-item identifier as required by `AudioPlayerItemProtocol`.
    public nonisolated let audioId: String

    /// Numeric form of ``audioId``, derived from the first 16 hex
    /// digits of the UUID. Stable for a given UUID; not unique against
    /// arbitrary collisions.
    public nonisolated var uuid: Int64 { _inner.uuidI64() }

    /// The source URL.
    public nonisolated let url: URL

    /// Caller-declared live-stream flag, mirrored from the construction
    /// parameter.
    public nonisolated var isLiveStream: Bool { _inner.isLiveStream() }

    /// Cached duration in seconds. Defaults to `0` until the underlying
    /// resource emits a duration update.
    public nonisolated var durationSec: TimeInterval { _inner.durationSec() }

    // MARK: - Event stream

    private let _eventSubject = PassthroughSubject<ItemEvent, Never>()

    /// Single stream of all item events from Rust.
    public nonisolated var eventPublisher: AnyPublisher<ItemEvent, Never> {
        _eventSubject.eraseToAnyPublisher()
    }

    // MARK: - Bitrate preferences

    /// Preferred peak bitrate in bits per second. Zero means no limit.
    public nonisolated var preferredPeakBitrate: Double {
        _inner.preferredPeakBitrate()
    }

    /// Preferred peak bitrate for expensive networks in bits per second. Zero means no limit.
    public nonisolated var preferredPeakBitrateForExpensiveNetworks: Double {
        _inner.preferredPeakBitrateForExpensiveNetworks()
    }

    // MARK: - Internal

    nonisolated let _inner: AudioPlayerItem

    // MARK: - Init

    /// Create a new item.
    ///
    /// - Parameters:
    ///   - url: The audio source URL.
    ///   - additionalHeaders: Optional HTTP headers included in all requests for this item.
    ///   - preferredPeakBitrate: Peak bitrate ceiling in bits/sec. `0` means no cap.
    ///   - preferredPeakBitrateForExpensiveNetworks: Peak bitrate ceiling on
    ///     expensive networks (cellular). `0` means no cap.
    ///   - abrMode: Optional per-item ABR mode override.
    ///   - isLiveStream: `true` for live HLS feeds; reported through
    ///     ``isLiveStream`` and consumed by ``isPlayable(progress:ranges:)``.
    public init(
        url: String,
        additionalHeaders: [String: String]? = nil,
        preferredPeakBitrate: Double = 0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0,
        abrMode: AbrMode? = nil,
        isLiveStream: Bool = false
    ) {
        let ffiAbrMode: FfiAbrMode? = abrMode.map {
            switch $0 {
            case .auto: return .auto
            case .manual(let variantIndex): return .manual(variantIndex: UInt32(variantIndex))
            }
        }
        let config = FfiItemConfig(
            abrMode: ffiAbrMode,
            headers: additionalHeaders,
            url: url,
            preferredPeakBitrate: preferredPeakBitrate,
            preferredPeakBitrateExpensive: preferredPeakBitrateForExpensiveNetworks,
            isLiveStream: isLiveStream
        )
        self._inner = AudioPlayerItem(config: config)
        self.audioId = _inner.audioId()
        self.url = URL(string: url) ?? URL(fileURLWithPath: url)

        let observer = ItemObserverBridge(subject: _eventSubject)
        _inner.setObserver(observer: observer)
    }

    /// Internal init wrapping an existing FFI item (used by ``KitharaPlayer/items``).
    init(inner: AudioPlayerItem) {
        self._inner = inner
        self.audioId = inner.audioId()
        let urlString = inner.url()
        self.url = URL(string: urlString) ?? URL(fileURLWithPath: urlString)
    }

    // MARK: - Load / playability

    /// Resolve a ``ItemLoadResult`` describing the item's current load
    /// status. The result reflects cached state — `KitharaPlayer.insert`
    /// already kicks off background loading. This method mirrors the
    /// iOS `AudioPlayerItemProtocol.load()` Observable contract,
    /// translated to Swift's `async`/`await`.
    public func load() async -> ItemLoadResult {
        await withCheckedContinuation { continuation in
            let bridge = ItemLoadCallbackBridge { result in
                continuation.resume(returning: result)
            }
            _inner.load(callback: bridge)
        }
    }

    /// Whether the item is playable at `progress` (seconds) given the
    /// caller-supplied buffered `ranges`. Live streams are reported
    /// playable unconditionally.
    public func isPlayable(progress: Double, ranges: [ItemLoadedRange]) -> Bool {
        let ffiRanges = ranges.map { range in
            FfiTimeRange(durationSeconds: range.duration, startSeconds: range.start)
        }
        return _inner.isPlayable(progress: progress, ranges: ffiRanges)
    }
}

// MARK: - Public value types

/// Outcome reported by ``KitharaPlayerItem/load()``.
public struct ItemLoadResult: Sendable, Equatable {
    /// `true` once the metadata layer recognises encrypted segments.
    public let hasProtectedContent: Bool
    /// `true` when the item has enough metadata to start playback.
    public let isPlayable: Bool

    init(ffi: FfiItemLoadResult) {
        self.hasProtectedContent = ffi.hasProtectedContent
        self.isPlayable = ffi.isPlayable
    }
}

/// Buffered range expressed as `[start, start + duration)` seconds.
public struct ItemLoadedRange: Sendable, Equatable {
    public let start: Double
    public let duration: Double

    public init(start: Double, duration: Double) {
        self.start = start
        self.duration = duration
    }

    init(ffi: FfiTimeRange) {
        self.start = ffi.startSeconds
        self.duration = ffi.durationSeconds
    }
}

// MARK: - ItemObserver bridge (single on_event callback)

private final class ItemObserverBridge: KitharaFFI.ItemObserver, @unchecked Sendable {
    private let subject: PassthroughSubject<ItemEvent, Never>

    init(subject: PassthroughSubject<ItemEvent, Never>) {
        self.subject = subject
    }

    func onEvent(event: FfiItemEvent) {
        subject.send(event)
    }
}

// MARK: - ItemLoadCallback bridge

private final class ItemLoadCallbackBridge: KitharaFFI.ItemLoadCallback, @unchecked Sendable {
    private let handler: (ItemLoadResult) -> Void

    init(handler: @escaping (ItemLoadResult) -> Void) {
        self.handler = handler
    }

    func onComplete(result: FfiItemLoadResult) {
        handler(ItemLoadResult(ffi: result))
    }
}
