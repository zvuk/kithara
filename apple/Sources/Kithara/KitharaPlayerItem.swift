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
///
/// `open` for additive subclassing (extra state, methods, protocol
/// conformances). Methods stay `public`, not `open`, so item behavior cannot
/// be overridden. Subclasses construct via the public `init(url:…)` and must
/// preserve the `@unchecked Sendable` contract: no non-`Sendable` mutable state.
open class KitharaPlayerItem: KitharaPlayerItemProtocol, @unchecked Sendable {
    /// Unique queue-item identity for SwiftUI lists.
    public nonisolated var id: Int64 { uuid }

    /// Caller-facing content track id.
    public nonisolated let audioId: TrackId

    /// Internal id allocated by the Rust queue layer.
    nonisolated let ffiTrackId: KitharaFFI.TrackId

    /// Unique queue-item handle. Distinct from ``audioId`` when the same
    /// content track is inserted more than once.
    public nonisolated var uuid: Int64 { _inner.uuidI64() }

    /// The source URL — `file://…` for absolute local paths.
    public nonisolated let url: URL

    /// Whether the source is an endless live stream.
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

    // MARK: - Discrete publishers (KitharaPlayerItemProtocol)

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

    /// HLS variant ladder discovered for this item. Fires once per
    /// master-playlist parse with the full sorted-by-bandwidth list.
    /// Use this to populate a quality picker UI.
    public nonisolated var variantsDiscovered: AnyPublisher<[Variant], Never> {
        _eventSubject
            .compactMap { event -> [Variant]? in
                if case let .variantsDiscovered(variants) = event {
                    return variants.map(Variant.init(ffi:))
                }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Variant the ABR controller has *chosen* but not yet applied —
    /// emits before the next segment fetch on that variant.
    public nonisolated var variantSelected: AnyPublisher<Variant, Never> {
        _eventSubject
            .compactMap { event -> Variant? in
                if case let .variantSelected(variant) = event {
                    return Variant(ffi: variant)
                }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Variant the decoder is *currently* feeding to the audio
    /// pipeline. Emits when the segment from a newly-selected variant
    /// crosses the playback head — i.e. when the user audibly hears
    /// the new quality.
    public nonisolated var variantApplied: AnyPublisher<Variant, Never> {
        _eventSubject
            .compactMap { event -> Variant? in
                if case let .variantApplied(variant) = event {
                    return Variant(ffi: variant)
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
    ///   - audioId: Caller-facing content track id. If `nil`, Kithara
    ///     uses the internally allocated queue id converted to `Int`.
    ///   - uuid: Caller-facing queue item id. If `nil`, Kithara uses
    ///     its legacy generated handle.
    ///   - additionalHeaders: Optional HTTP headers included in all requests for this item.
    ///   - preferredPeakBitrate: Peak bitrate ceiling in bits/sec. `0` means no cap.
    ///   - preferredPeakBitrateForExpensiveNetworks: Peak bitrate ceiling on
    ///     expensive networks (cellular). `0` means no cap.
    ///   - abrMode: Optional per-item ABR mode override.
    public init(
        url: String,
        audioId: TrackId? = nil,
        uuid: Int64? = nil,
        additionalHeaders: [String: String]? = nil,
        preferredPeakBitrate: Double = 0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0,
        abrMode: AbrMode? = nil
    ) {
        let ffiAbrMode: FfiAbrMode? = abrMode.map {
            switch $0 {
            case .auto: return .auto
            case .manual(let variantIndex): return .manual(variantIndex: UInt32(variantIndex))
            }
        }
        let config = FfiItemConfig(
            abrMode: ffiAbrMode,
            audioId: audioId.map { Self.ffiTrackId(from: $0) },
            headers: additionalHeaders,
            uuidI64: uuid,
            url: url,
            isLiveStream: false,
            preferredPeakBitrate: preferredPeakBitrate,
            preferredPeakBitrateExpensive: preferredPeakBitrateForExpensiveNetworks
        )
        self._inner = AudioPlayerItem(config: config)
        self.ffiTrackId = _inner.queueId()
        self.audioId = TrackId(ffi: _inner.audioId())
        self.url = URL(string: url) ?? URL(fileURLWithPath: url)

        let observer = ItemObserverBridge(subject: _eventSubject)
        _inner.setObserver(observer: observer)
    }

    /// Internal init wrapping an existing FFI item (used by ``KitharaPlayer/items``).
    init(inner: AudioPlayerItem) {
        self._inner = inner
        self.ffiTrackId = inner.queueId()
        self.audioId = TrackId(ffi: inner.audioId())
        let urlString = inner.url()
        self.url = URL(string: urlString) ?? URL(fileURLWithPath: urlString)

        let observer = ItemObserverBridge(subject: _eventSubject)
        inner.setObserver(observer: observer)
    }

    private static func ffiTrackId(from id: TrackId) -> KitharaFFI.TrackId {
        guard let converted = KitharaFFI.TrackId(exactly: id) else {
            preconditionFailure("Kithara TrackId \(id) cannot be represented as KitharaFFI.TrackId")
        }
        return converted
    }

    // MARK: - Load / playability

    /// Resolve a ``ItemLoadResult`` describing the item's current load
    /// status. The result reflects cached state — `KitharaPlayer.insert`
    /// already kicks off background loading.
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

    /// `and:`-labelled overload for call sites that prefer that spelling.
    /// Delegates to ``isPlayable(progress:ranges:)``.
    public func isPlayable(progress: Double, and ranges: [ItemLoadedRange]) -> Bool {
        isPlayable(progress: progress, ranges: ranges)
    }

    /// Whether the item is playable for a structured playback progress.
    public func isPlayable(progress: PlaybackProgress, ranges: [ItemLoadedRange]) -> Bool {
        isPlayable(progress: progress, ranges: ranges, startTolerance: .zero)
    }

    /// Whether the item is playable for a structured playback progress.
    ///
    /// `startTolerance` accepts a buffered range whose start lands slightly
    /// after `progress.time`, which is useful for HLS seek readiness policies.
    public func isPlayable(
        progress: PlaybackProgress,
        ranges: [ItemLoadedRange],
        startTolerance: TimeInterval
    ) -> Bool {
        if isLiveStream {
            return true
        }
        if progress.value >= 1, !ranges.isEmpty {
            return true
        }
        let tolerance = max(TimeInterval.zero, startTolerance)
        let adjustedRanges = ranges.map { range in
            ItemLoadedRange(
                start: range.start - tolerance,
                duration: range.duration + tolerance
            )
        }
        return isPlayable(progress: progress.time, ranges: adjustedRanges)
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
    /// Range start in seconds from the item's timeline origin.
    public let start: Double
    /// Range length in seconds.
    public let duration: Double

    /// Construct a range from its start + length in seconds.
    public init(start: Double, duration: Double) {
        self.start = start
        self.duration = duration
    }

    init(ffi: FfiTimeRange) {
        self.start = ffi.startSeconds
        self.duration = ffi.durationSeconds
    }

    public init<Source: ItemLoadedRangeSource>(_ source: Source) {
        self.start = source.start
        self.duration = source.duration
    }
}

/// Structured playback progress for queue-readiness checks.
public struct PlaybackProgress: Sendable, Equatable {
    /// Normalised playback position in `[0, 1]`.
    public let value: TimeInterval
    /// Playback position in seconds.
    public let time: TimeInterval
    /// Item duration in seconds.
    public let duration: TimeInterval

    public init(value: TimeInterval, time: TimeInterval, duration: TimeInterval) {
        self.value = value
        self.time = time
        self.duration = duration
    }

    public init<Source: PlaybackProgressSource>(_ source: Source) {
        self.value = source.value
        self.time = source.time
        self.duration = source.duration
    }
}

public protocol ItemLoadedRangeSource {
    var start: TimeInterval { get }
    var duration: TimeInterval { get }
}

public protocol PlaybackProgressSource {
    var value: TimeInterval { get }
    var time: TimeInterval { get }
    var duration: TimeInterval { get }
}

extension ItemLoadedRange: ItemLoadedRangeSource {}
extension PlaybackProgress: PlaybackProgressSource {}

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
