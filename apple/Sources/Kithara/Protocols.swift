import Combine
import Foundation
import KitharaFFI

// MARK: - KitharaPlayerItemProtocol

/// Protocol matching Kithara's Swift item surface.
public protocol KitharaPlayerItemProtocol: AnyObject, Identifiable, Sendable {
    /// Caller-facing content track id.
    var audioId: TrackId { get }

    /// Unique queue-item handle. Distinct from ``audioId`` when the same
    /// content track is inserted more than once.
    var uuid: Int64 { get }

    /// The source URL (`file://…` for local paths).
    var url: URL { get }

    /// Whether the source is an endless live stream.
    var isLiveStream: Bool { get }

    /// Cached duration in seconds. `0` until duration metadata arrives.
    var durationSec: TimeInterval { get }

    /// Preferred peak bitrate for HLS (0 = no limit). Frozen at item
    /// construction.
    var preferredPeakBitrate: Double { get }

    /// Preferred peak bitrate for expensive (cellular) networks.
    /// Frozen at item construction.
    var preferredPeakBitrateForExpensiveNetworks: Double { get }

    /// Buffered byte ranges. Each emission is the full set of buffered ranges.
    var loadedRanges: AnyPublisher<[ItemLoadedRange], Never> { get }

    /// Item duration publisher (seconds). Emits `nil` while unknown.
    var duration: AnyPublisher<Double?, Never> { get }

    /// Error publisher. Combine equivalent of iOS `rxError`.
    var error: AnyPublisher<Error, Never> { get }

    /// Current bitrate in bits/sec (latest applied variant).
    var bitrate: AnyPublisher<Int32, Never> { get }

    /// Fires once the metadata layer reports `ReadyToPlay`.
    var readyToPlay: AnyPublisher<Void, Never> { get }

    /// Fires when the item plays out to its natural end.
    var didReachEnd: AnyPublisher<Void, Never> { get }

    /// Fires when playback stalls.
    var didStall: AnyPublisher<Void, Never> { get }

    /// Resolves once the item's metadata layer reports playable state.
    func load() async -> ItemLoadResult

    /// Whether the item is playable at `progress` (seconds) given the
    /// caller-supplied buffered `ranges`. Live streams are reported
    /// playable unconditionally.
    func isPlayable(progress: Double, ranges: [ItemLoadedRange]) -> Bool

    /// Whether the item is playable for structured queue progress.
    func isPlayable(progress: PlaybackProgress, ranges: [ItemLoadedRange]) -> Bool

    /// Whether the item is playable for structured queue progress with an
    /// explicit buffered-range start tolerance.
    func isPlayable(
        progress: PlaybackProgress,
        ranges: [ItemLoadedRange],
        startTolerance: TimeInterval
    ) -> Bool

    /// `and:`-labeled variant of ``isPlayable(progress:ranges:)``.
    func isPlayable(progress: Double, and ranges: [ItemLoadedRange]) -> Bool
}

public extension KitharaPlayerItemProtocol {
    func isPlayable<ProgressSource: PlaybackProgressSource, RangeSource: ItemLoadedRangeSource>(
        progress: ProgressSource,
        ranges: [RangeSource]
    ) -> Bool {
        isPlayable(progress: progress, ranges: ranges, startTolerance: .zero)
    }

    func isPlayable<ProgressSource: PlaybackProgressSource, RangeSource: ItemLoadedRangeSource>(
        progress: ProgressSource,
        ranges: [RangeSource],
        startTolerance: TimeInterval
    ) -> Bool {
        isPlayable(
            progress: PlaybackProgress(progress),
            ranges: ranges.map { ItemLoadedRange($0) },
            startTolerance: startTolerance
        )
    }
}

// MARK: - KitharaPlayerProtocol

/// Protocol matching Kithara's queue-based playback surface.
public protocol KitharaPlayerProtocol: AnyObject, Sendable {
    /// Concrete queue-item type the conforming player drives. The
    /// stock implementation pins this to ``KitharaPlayerItem``.
    associatedtype Item: KitharaPlayerItemProtocol

    /// Playback volume (0.0–1.0, clamped).
    var volume: Float { get set }

    /// Target playback speed used by ``play()``. While playing,
    /// ``rate`` equals this value; on pause `rate` drops to `0`.
    /// Mirrors iOS `playingRate`.
    var playingRate: Float { get set }

    /// Synchronous current playback time in seconds. Drops to `0`
    /// when the current item is removed. Mirrors iOS sync
    /// `currentTime: Double`.
    var currentTime: TimeInterval { get }

    /// Current queue item visible to iOS-style clients, or `nil` when
    /// the queue is empty.
    var currentAudioItem: Item? { get }

    /// Current item publisher. Emits the first queued item before
    /// playback starts and re-emits when the visible current item
    /// changes. Combine equivalent of iOS `rxCurrentAudioItem`.
    var currentItem: AnyPublisher<Item?, Never> { get }

    /// Live playback rate publisher. Combine equivalent of iOS
    /// `rxRate`.
    var rate: AnyPublisher<Float, Never> { get }

    /// Error publisher. Combine equivalent of iOS `rxError`.
    var error: AnyPublisher<Error, Never> { get }

    /// The current playback queue. iOS-style `func`, not a property.
    func items() -> [Item]

    /// Insert an item into the queue. If `afterItem` is `nil`, the
    /// item is placed at the head (iOS contract — *not* AVQueuePlayer
    /// append). Use ``KitharaPlayer/insert(_:after:)``'s overload that
    /// appends when you need AVQueuePlayer semantics.
    func insert(_ item: Item, after afterItem: Item?) throws

    /// Remove an item from the queue.
    func remove(_ item: Item) throws

    /// Skip to the next item in the queue. No-op if already on the
    /// last item or the queue is empty.
    func advanceToNextItem()

    /// Remove all items from the queue.
    func removeAllItems()

    /// Stop playback, clear the queue, and reset the current-item slot.
    func stop()

    /// Start or resume playback.
    func play()

    /// Pause playback.
    func pause()

    /// Notify Kithara that the platform audio route changed.
    func notifyAudioRouteChanged(reason: String)

    /// Seek to a position with optional landing tolerance (advisory).
    func seek(
        to: Double,
        tolerance: Double?,
        completionHandler: @escaping SeekCompletionHandler
    )

    /// Per-network bitrate ceilings (bits/sec). Pass `0` for either
    /// argument to lift that limit.
    func updatePeakBitrate(wifi: Double, cellular: Double)

    /// Configure the auth token sent on every player HTTP request.
    func setupNetwork(authToken: String)

    /// Register a runtime DRM key decryptor on every host (`"*"`).
    func setupHlsAes(keyDecryptor: @escaping (Data, String) -> Data?)
}
