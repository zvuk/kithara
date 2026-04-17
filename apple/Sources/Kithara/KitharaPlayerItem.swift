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
    public init(
        url: String,
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
            url: url,
            headers: additionalHeaders,
            preferredPeakBitrate: preferredPeakBitrate,
            preferredPeakBitrateExpensive: preferredPeakBitrateForExpensiveNetworks,
            abrMode: ffiAbrMode
        )
        self._inner = AudioPlayerItem(config: config)
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
