import Combine
import Foundation
import KitharaFFI

/// A queue-based audio player with Combine-driven observation.
///
/// Thin wrapper over the Rust ``AudioPlayer``. All state lives in Rust —
/// Swift queries it on demand via ``snapshot()`` or receives push
/// updates through ``eventPublisher``.
///
/// ```swift
/// let player = KitharaPlayer()
/// let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
/// item.load()
/// try player.insert(item)
/// player.play()
/// ```
public final class KitharaPlayer: @unchecked Sendable {
    private let _inner: AudioPlayer
    private let _eventSubject = PassthroughSubject<PlayerEvent, Never>()

    // MARK: - Event stream

    /// Single stream of all player events from Rust.
    public var eventPublisher: AnyPublisher<PlayerEvent, Never> {
        _eventSubject.eraseToAnyPublisher()
    }

    // MARK: - State (queried from Rust on demand)

    /// Snapshot of the player's current state.
    public var snapshot: PlayerSnapshot {
        _inner.snapshot()
    }

    /// Current player status.
    public var status: PlayerStatus {
        PlayerStatus(ffi: _inner.snapshot().status)
    }

    /// Current playback time in seconds, or `nil` if no item is loaded.
    public var currentTime: TimeInterval? {
        _inner.snapshot().currentTime
    }

    /// Current item duration in seconds, or `nil` if unknown.
    public var duration: TimeInterval? {
        _inner.snapshot().duration
    }

    // MARK: - Rate

    /// Current playback rate (1.0 = normal speed).
    public var rate: Float {
        _inner.rate()
    }

    /// Default playback rate used when ``play()`` is called.
    public var defaultRate: Float {
        get { _inner.defaultRate() }
        set { _inner.setDefaultRate(rate: newValue) }
    }

    // MARK: - Volume & Mute

    /// Playback volume (0.0–1.0, clamped).
    public var volume: Float {
        get { _inner.volume() }
        set { _inner.setVolume(volume: newValue) }
    }

    /// Whether the player is muted.
    public var isMuted: Bool {
        get { _inner.isMuted() }
        set { _inner.setMuted(muted: newValue) }
    }

    // MARK: - EQ

    /// Number of EQ bands (fixed at creation).
    public var eqBandCount: Int {
        Int(_inner.eqBandCount())
    }

    /// Get the current gain for a band in dB.
    public func eqGain(band: Int) -> Float {
        _inner.eqGain(band: UInt32(band))
    }

    /// Set the gain for a band in dB (-24..+6).
    public func setEqGain(band: Int, gainDb: Float) {
        try? _inner.setEqGain(band: UInt32(band), gainDb: gainDb)
    }

    /// Reset all EQ bands to 0 dB.
    public func resetEq() {
        try? _inner.resetEq()
    }

    /// Change ABR mode at runtime.
    public func setAbrMode(_ mode: AbrMode) {
        let ffiMode: FfiAbrMode = switch mode {
        case .auto:
            .auto
        case .manual(let index):
            .manual(variantIndex: UInt32(index))
        }
        _inner.setAbrMode(mode: ffiMode)
    }

    // MARK: - Init

    /// Configuration for player creation.
    public struct Config: Sendable {
        /// Number of EQ bands (log-spaced). Default: 10.
        public var eqBandCount: Int

        public init(eqBandCount: Int = 10) {
            self.eqBandCount = eqBandCount
        }
    }

    /// Create a new player instance.
    public init(config: Config = Config()) {
        self._inner = AudioPlayer(config: FfiPlayerConfig(eqBandCount: UInt32(config.eqBandCount)))

        let bridge = PlayerObserverBridge(subject: _eventSubject)
        _inner.setObserver(observer: bridge)
    }

    // MARK: - Playback control

    /// Start or resume playback at the default rate.
    public func play() {
        _inner.play()
    }

    /// Pause playback.
    public func pause() {
        _inner.pause()
    }

    /// Seek to a position in the current item.
    ///
    /// - Parameters:
    ///   - seconds: Target time in seconds.
    ///   - callback: Invoked with `true` if the seek was accepted, `false` otherwise.
    public func seek(to seconds: TimeInterval, callback: SeekCallback) {
        _inner.seek(toSeconds: seconds, callback: callback)
    }

    // MARK: - Queue management (delegated to Rust)

    /// Items inserted via ``insert(_:after:)``, preserving Swift identity.
    private var _knownItems: [String: KitharaPlayerItem] = [:]

    /// The current playback queue.
    ///
    /// Returns the same Swift instances that were passed to ``insert(_:after:)``,
    /// preserving identity and active event publishers.
    public var items: [KitharaPlayerItem] {
        let ffiItems = _inner.items()
        return ffiItems.compactMap { ffiItem in
            let id = ffiItem.id()
            return _knownItems[id]
        }
    }

    /// Insert an item into the queue.
    ///
    /// - Parameters:
    ///   - item: The item to insert. May be loaded or not yet loaded (auto-load).
    ///   - after: Insert after this item. If `nil`, appends to the end.
    /// - Throws: ``KitharaError`` if `after` is not in the queue.
    public func insert(_ item: KitharaPlayerItem, after: KitharaPlayerItem? = nil) throws {
        do {
            try _inner.insert(item: item._inner, after: after?._inner)
            _knownItems[item.id] = item
        } catch let ffiError as FfiError {
            throw KitharaError(ffi: ffiError)
        }
    }

    /// Remove an item from the queue.
    ///
    /// - Parameter item: The item to remove.
    /// - Throws: ``KitharaError`` if the item is not in the queue.
    public func remove(_ item: KitharaPlayerItem) throws {
        do {
            try _inner.remove(item: item._inner)
            _knownItems.removeValue(forKey: item.id)
        } catch let ffiError as FfiError {
            throw KitharaError(ffi: ffiError)
        }
    }

    /// Remove all items from the queue.
    public func removeAllItems() {
        _inner.removeAllItems()
        _knownItems.removeAll()
    }

    // MARK: - Key processing (DRM)

    /// Set a key processor for HLS DRM key decryption.
    ///
    /// The processor is applied to all items' key requests when loading.
    /// Optional `headers` are sent with every key request (e.g. `X-Encrypted-Key`).
    ///
    /// - Parameters:
    ///   - processor: Callback that receives encrypted key bytes and returns decrypted bytes.
    ///   - headers: Optional HTTP headers for key requests only.
    public func setKeyProcessor(_ processor: KeyProcessor, headers: [String: String]? = nil) {
        let bridge = KeyProcessorBridge(processor: processor)
        _inner.setKeyProcessor(processor: bridge, headers: headers)
    }

}

// MARK: - Key processor

/// Callback for processing (decrypting) HLS encryption keys.
///
/// Implement this protocol to provide custom key decryption logic.
/// The player calls ``processKey(_:)`` for each key fetched from the server.
public protocol KeyProcessor: Sendable {
    /// Process (decrypt) raw key bytes received from the server.
    ///
    /// - Parameter key: Encrypted key bytes.
    /// - Returns: Decrypted key bytes.
    func processKey(_ key: Data) -> Data
}

private final class KeyProcessorBridge: KitharaFFI.FfiKeyProcessor, @unchecked Sendable {
    private let processor: KeyProcessor

    init(processor: KeyProcessor) {
        self.processor = processor
    }

    func processKey(key: Data) -> Data {
        processor.processKey(key)
    }
}

// MARK: - PlayerObserver bridge (single on_event callback)

private final class PlayerObserverBridge: KitharaFFI.PlayerObserver, @unchecked Sendable {
    private let subject: PassthroughSubject<PlayerEvent, Never>

    init(subject: PassthroughSubject<PlayerEvent, Never>) {
        self.subject = subject
    }

    func onEvent(event: FfiPlayerEvent) {
        subject.send(event)
    }
}
