import Combine
import Foundation
import KitharaFFI

// MARK: - AudioPlayerItemProtocol

/// Protocol matching AVPlayerItem-style semantics for audio items.
/// Combine equivalent of the iOS `AudioPlayerItemProtocol` — each
/// `rxFoo: Observable<T>` slot in the iOS contract becomes a typed
/// `foo: AnyPublisher<T, Never>` slot here.
public protocol AudioPlayerItemProtocol: AnyObject, Identifiable, Sendable {
    /// Monotonic per-item identifier — same `TrackId` the queue uses
    /// internally. Mirrors `AudioPlayerItemProtocol.audioId: TrackId`
    /// on iOS.
    var audioId: TrackId { get }

    /// Secondary handle derived from `url + audioId` (UUIDv5 → first
    /// 64 bits). Distinct from `audioId` for two items with the same
    /// URL but different queue insertions. Mirrors
    /// `AudioPlayerItemProtocol.uuid: Int64` on iOS.
    var uuid: Int64 { get }

    /// The source URL (`file://…` for local paths).
    var url: URL { get }

    /// Caller-declared live-stream flag.
    var isLiveStream: Bool { get }

    /// Cached duration in seconds. `0` until duration metadata arrives.
    var durationSec: TimeInterval { get }

    /// Preferred peak bitrate for HLS (0 = no limit). Frozen at item
    /// construction.
    var preferredPeakBitrate: Double { get }

    /// Preferred peak bitrate for expensive (cellular) networks.
    /// Frozen at item construction.
    var preferredPeakBitrateForExpensiveNetworks: Double { get }

    /// Buffered byte ranges. Each emission is the full set of buffered
    /// ranges. Combine equivalent of iOS `rxLoadedRanges`.
    var loadedRanges: AnyPublisher<[ItemLoadedRange], Never> { get }

    /// Item duration publisher (seconds). Emits `nil` while unknown.
    var duration: AnyPublisher<Double?, Never> { get }

    /// Error publisher. Combine equivalent of iOS `rxError`.
    var error: AnyPublisher<Error, Never> { get }

    /// Current bitrate in bits/sec (latest applied variant). Combine
    /// equivalent of iOS `rxBitrate`.
    var bitrate: AnyPublisher<Int32, Never> { get }

    /// Fires once the metadata layer reports `ReadyToPlay`. Combine
    /// equivalent of iOS `rxReadyToPlay`.
    var readyToPlay: AnyPublisher<Void, Never> { get }

    /// Fires when the item plays out to its natural end. Combine
    /// equivalent of iOS `rxDidReachEnd`.
    var didReachEnd: AnyPublisher<Void, Never> { get }

    /// Fires when playback stalls (waiting for more data). Combine
    /// equivalent of iOS `rxDidStall`.
    var didStall: AnyPublisher<Void, Never> { get }

    /// Resolves once the item's metadata layer reports playable state.
    /// Mirrors the iOS `func load() -> Observable<ItemLoadResult>`
    /// shape via Swift Concurrency.
    func load() async -> ItemLoadResult

    /// Whether the item is playable at `progress` (seconds) given the
    /// caller-supplied buffered `ranges`. Live streams are reported
    /// playable unconditionally.
    func isPlayable(progress: Double, ranges: [ItemLoadedRange]) -> Bool

    /// `and:`-labeled variant of ``isPlayable(progress:ranges:)``.
    /// Mirrors the iOS `AudioPlayerItemProtocol.isPlayable(progress:and:)`
    /// external label.
    func isPlayable(progress: Double, and ranges: [ItemLoadedRange]) -> Bool
}

// MARK: - AudioPlayerProtocol

/// Protocol matching AVPlayer-style semantics for queue-based playback.
/// Combine equivalent of the iOS `AudioPlayerProtocol`: every `rxFoo`
/// Observable on the iOS side becomes a typed `foo: AnyPublisher`
/// here; sync getters and setters keep their iOS names.
public protocol AudioPlayerProtocol: AnyObject, Sendable {
    /// Concrete queue-item type the conforming player drives. The
    /// stock implementation pins this to ``KitharaPlayerItem``.
    associatedtype Item: AudioPlayerItemProtocol

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

    /// Currently playing item, or `nil` when the queue is empty.
    var currentAudioItem: Item? { get }

    /// Current item publisher — re-emits when the engine flips to a
    /// new track. Combine equivalent of iOS `rxCurrentAudioItem`.
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

    /// Seek to a position with optional landing tolerance (advisory).
    func seek(
        to: Double,
        tolerance: Double?,
        completionHandler: SeekCallback
    )

    /// Per-network bitrate ceilings (bits/sec). Pass `0` for either
    /// argument to lift that limit.
    func updatePeakBitrate(wifi: Double, cellular: Double)

    /// Configure the auth token sent on every player HTTP request.
    func setupNetwork(authToken: String)

    /// Register a runtime DRM key decryptor on every host (`"*"`).
    func setupHlsAes(keyDecryptor: @escaping (Data, String) -> Data?)
}
