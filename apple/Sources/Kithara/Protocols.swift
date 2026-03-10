import Combine
import Foundation
import KitharaFFI

// MARK: - AudioPlayerItemProtocol

/// Protocol matching AVPlayerItem-style semantics for audio items.
public protocol AudioPlayerItemProtocol: AnyObject, Identifiable, Sendable {
    /// Preferred peak bitrate for HLS (0 = no limit).
    var preferredPeakBitrate: Double { get set }

    /// Preferred peak bitrate for expensive (cellular) networks.
    var preferredPeakBitrateForExpensiveNetworks: Double { get set }

    /// Buffered duration publisher.
    var bufferedDuration: AnyPublisher<Double?, Never> { get }

    /// Item duration publisher.
    var duration: AnyPublisher<Double?, Never> { get }

    /// Error event publisher.
    var error: AnyPublisher<Error, Never> { get }

    /// Start loading the underlying resource (fire-and-forget).
    func load()
}

// MARK: - AudioPlayerProtocol

/// Protocol matching AVPlayer-style semantics for queue-based playback.
public protocol AudioPlayerProtocol: AnyObject, Sendable {
    associatedtype Item: AudioPlayerItemProtocol

    /// Playback volume (0.0–1.0, clamped).
    var volume: Float { get set }

    /// Whether the player is muted.
    var isMuted: Bool { get set }

    /// Default playback rate used by `play()`.
    var defaultRate: Float { get set }

    /// Current item changed publisher (item ID).
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

    /// Remove all items from the queue.
    func removeAllItems()

    /// Start or resume playback.
    func play()

    /// Pause playback.
    func pause()

    /// Seek to a position.
    /// - Parameters:
    ///   - to: Target position in seconds.
    ///   - completionHandler: Called with `true` if seek was accepted.
    func seek(to: Double, completionHandler: SeekCallback)
}
