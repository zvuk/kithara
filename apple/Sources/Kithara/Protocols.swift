import Combine
import Foundation
import KitharaFFI

// MARK: - AudioPlayerItemProtocol

/// Protocol matching AVPlayerItem-style semantics for audio items.
public protocol AudioPlayerItemProtocol: AnyObject, Identifiable, Sendable {
    /// Stable per-item identifier (UUID-string).
    var audioId: String { get }

    /// Numeric form of ``audioId`` — derived from the first 16 hex
    /// digits of the UUID. Stable per-session, opaque otherwise.
    var uuid: Int64 { get }

    /// The source URL.
    var url: URL { get }

    /// Caller-declared live-stream flag.
    var isLiveStream: Bool { get }

    /// Cached duration in seconds. `0` until duration metadata arrives.
    var durationSec: TimeInterval { get }

    /// Preferred peak bitrate for HLS (0 = no limit).
    var preferredPeakBitrate: Double { get set }

    /// Preferred peak bitrate for expensive (cellular) networks.
    var preferredPeakBitrateForExpensiveNetworks: Double { get set }

    /// Buffered byte ranges as Combine publisher. Each emission is the
    /// full set of buffered ranges (caller-side aggregation is not
    /// required).
    var loadedRanges: AnyPublisher<[ItemLoadedRange], Never> { get }

    /// Item duration publisher (seconds). Emits `nil` while unknown.
    var duration: AnyPublisher<Double?, Never> { get }

    /// Error event publisher.
    var error: AnyPublisher<Error, Never> { get }

    /// Resolves once the item's metadata layer reports playable state.
    /// Mirrors the iOS `func load() -> Observable<ItemLoadResult>`
    /// shape via Swift Concurrency.
    func load() async -> ItemLoadResult

    /// Whether the item is playable at `progress` (seconds) given the
    /// caller-supplied buffered `ranges`. Live streams are reported
    /// playable unconditionally.
    func isPlayable(progress: Double, ranges: [ItemLoadedRange]) -> Bool
}

// MARK: - AudioPlayerProtocol

/// Protocol matching AVPlayer-style semantics for queue-based playback.
public protocol AudioPlayerProtocol: AnyObject, Sendable {
    associatedtype Item: AudioPlayerItemProtocol

    /// Playback volume (0.0–1.0, clamped).
    var volume: Float { get set }

    /// Whether the player is muted.
    var isMuted: Bool { get set }

    /// Target playback speed used by ``play()``. Mirrors
    /// `AudioPlayerProtocol.playingRate`.
    var playingRate: Float { get set }

    /// Currently playing item, or `nil` when the queue is empty.
    var currentAudioItem: Item? { get }

    /// Current item changed publisher.
    var currentItem: AnyPublisher<Item?, Never> { get }

    /// Current playback time publisher (seconds).
    var currentTime: AnyPublisher<Double, Never> { get }

    /// Current playback rate publisher.
    var rate: AnyPublisher<Float, Never> { get }

    /// Error publisher.
    var error: AnyPublisher<Error, Never> { get }

    /// The current playback queue.
    func items() -> [Item]

    /// Insert an item into the queue.
    /// If `afterItem` is `nil`, the item is appended (AVPlayer semantics).
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
