import Combine
import Foundation
import KitharaFFI

/// A single audio item that can be queued in ``KitharaPlayer``.
///
/// Create with a URL, then call ``load()`` before inserting into the player.
/// If inserted before loading completes, the player will auto-load the item.
///
/// ```swift
/// let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
/// item.load()
/// try player.insert(item)
/// ```
public final class KitharaPlayerItem: Identifiable, @unchecked Sendable {
    /// Unique item identifier.
    public nonisolated let id: String

    /// The source URL.
    public nonisolated let url: String

    // MARK: - Event stream

    private let _eventSubject = PassthroughSubject<ItemEvent, Never>()

    /// Single stream of all item events from Rust.
    public nonisolated var eventPublisher: AnyPublisher<ItemEvent, Never> {
        _eventSubject.eraseToAnyPublisher()
    }

    // MARK: - Bitrate preferences

    /// Preferred peak bitrate in bits per second. Zero means no limit.
    public nonisolated var preferredPeakBitrate: Double {
        get { _inner.preferredPeakBitrate() }
        set { _inner.setPreferredPeakBitrate(bitrate: newValue) }
    }

    /// Preferred peak bitrate for expensive networks in bits per second. Zero means no limit.
    public nonisolated var preferredPeakBitrateForExpensiveNetworks: Double {
        get { _inner.preferredPeakBitrateForExpensiveNetworks() }
        set { _inner.setPreferredPeakBitrateForExpensiveNetworks(bitrate: newValue) }
    }

    // MARK: - Internal

    nonisolated let _inner: AudioPlayerItem

    // MARK: - Init

    /// Create a new item for the given URL.
    ///
    /// - Parameters:
    ///   - url: The audio source URL.
    ///   - additionalHeaders: Optional HTTP headers included in all requests for this item.
    public init(url: String, additionalHeaders: [String: String]? = nil) {
        self._inner = AudioPlayerItem(url: url, additionalHeaders: additionalHeaders)
        self.id = _inner.id()
        self.url = url

        let observer = ItemObserverBridge(subject: _eventSubject)
        _inner.setObserver(observer: observer)
    }

    /// Internal init wrapping an existing FFI item (used by ``KitharaPlayer/items``).
    init(inner: AudioPlayerItem) {
        self._inner = inner
        self.id = inner.id()
        self.url = inner.url()
    }

    // MARK: - Loading

    /// Start loading the underlying resource (fire-and-forget).
    ///
    /// Errors are reported through ``eventPublisher`` as `.error` events.
    /// Safe to call before inserting into a ``KitharaPlayer`` — the player
    /// will also auto-load if the item is not yet ready.
    public func load() {
        _inner.load()
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
