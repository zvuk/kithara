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
public final class KitharaPlayerItem: AudioPlayerItemProtocol, @unchecked Sendable {
    /// Monotonic identifier shared with the queue. Mirrors iOS
    /// `AudioPlayerItemProtocol.audioId: TrackId`. Also used as
    /// `Identifiable.id` so `ForEach`/`List` keying works out of the
    /// box.
    public nonisolated var id: TrackId { audioId }

    /// Stable per-item identifier as required by `AudioPlayerItemProtocol`.
    public nonisolated let audioId: TrackId

    /// Secondary handle derived from `url + audioId` (UUIDv5 → first
    /// 64 bits). Distinct from ``audioId`` for two items with the same
    /// URL but different queue insertions. Mirrors iOS
    /// `AudioPlayerItemProtocol.uuid: Int64`.
    public nonisolated var uuid: Int64 { _inner.uuidI64() }

    /// The source URL — `file://…` for absolute local paths.
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

    // MARK: - Discrete publishers (AudioPlayerItemProtocol)

    /// Buffered byte ranges. Each emission is the full set. Combine
    /// equivalent of iOS `rxLoadedRanges`.
    public nonisolated var loadedRanges: AnyPublisher<[ItemLoadedRange], Never> {
        _eventSubject
            .compactMap { event -> [ItemLoadedRange]? in
                if case let .loadedRangesChanged(ranges) = event {
                    return ranges.map(ItemLoadedRange.init(ffi:))
                }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Duration publisher (seconds). Combine equivalent of an iOS
    /// `rxDuration` (not in their current protocol but useful here).
    public nonisolated var duration: AnyPublisher<Double?, Never> {
        _eventSubject
            .compactMap { event -> Double?? in
                if case let .durationChanged(seconds) = event {
                    return .some(seconds)
                }
                return nil
            }
            .map { $0 }
            .eraseToAnyPublisher()
    }

    /// Bitrate publisher (bits/sec, latest applied variant). Combine
    /// equivalent of iOS `rxBitrate`.
    public nonisolated var bitrate: AnyPublisher<Int32, Never> {
        _eventSubject
            .compactMap { event -> Int32? in
                if case let .variantApplied(variant) = event {
                    let bps = variant.bandwidthBps
                    return Int32(clamping: bps)
                }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Fires once the metadata layer reports `ReadyToPlay`. Combine
    /// equivalent of iOS `rxReadyToPlay`.
    public nonisolated var readyToPlay: AnyPublisher<Void, Never> {
        _eventSubject
            .compactMap { event -> Void? in
                if case let .statusChanged(status) = event, status == .readyToPlay {
                    return ()
                }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Fires when the item plays out to its natural end. Combine
    /// equivalent of iOS `rxDidReachEnd`.
    public nonisolated var didReachEnd: AnyPublisher<Void, Never> {
        _eventSubject
            .compactMap { event -> Void? in
                if case .didReachEnd = event { return () }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Fires when playback stalls. Combine equivalent of iOS
    /// `rxDidStall`.
    public nonisolated var didStall: AnyPublisher<Void, Never> {
        _eventSubject
            .compactMap { event -> Void? in
                if case .didStall = event { return () }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Error publisher. Maps `ItemEvent.error` / `ItemEvent.didFail`
    /// onto ``PlayerError``. Combine equivalent of iOS `rxError`.
    public nonisolated var error: AnyPublisher<Error, Never> {
        _eventSubject
            .compactMap { event -> PlayerError? in
                switch event {
                case let .error(message): return .itemFailed(message)
                case .didFail: return .itemFailed("item did fail")
                default: return nil
                }
            }
            .map { $0 as Error }
            .eraseToAnyPublisher()
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
            isLiveStream: isLiveStream,
            preferredPeakBitrate: preferredPeakBitrate,
            preferredPeakBitrateExpensive: preferredPeakBitrateForExpensiveNetworks
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

        let observer = ItemObserverBridge(subject: _eventSubject)
        inner.setObserver(observer: observer)
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

    /// `and:`-labelled overload mirroring the iOS
    /// `AudioPlayerItemProtocol.isPlayable(progress:and:)` signature.
    /// Delegates to ``isPlayable(progress:ranges:)``.
    public func isPlayable(progress: Double, and ranges: [ItemLoadedRange]) -> Bool {
        isPlayable(progress: progress, ranges: ranges)
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
