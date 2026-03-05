import Combine
import Foundation
import KitharaFFI

/// A single audio item that can be queued in ``KitharaPlayer``.
///
/// Create with a URL, then call ``load()`` before inserting into the player.
public final class KitharaPlayerItem: Identifiable, @unchecked Sendable {
    /// Unique item identifier.
    public nonisolated let id: String

    /// The source URL.
    public nonisolated let url: String

    // MARK: - Observable state

    /// Current item status.
    public nonisolated var status: ItemStatus {
        _state.withLock { $0.status }
    }

    /// Item duration in seconds, or `nil` if unknown.
    public nonisolated var duration: TimeInterval? {
        _state.withLock { $0.duration }
    }

    /// Buffered duration in seconds.
    public nonisolated var bufferedDuration: TimeInterval {
        _state.withLock { $0.bufferedDuration }
    }

    /// Last error, if status is ``ItemStatus/failed``.
    public nonisolated var error: KitharaError? {
        _state.withLock { $0.error }
    }

    // MARK: - Combine publishers

    /// Publishes item status changes.
    public nonisolated var statusPublisher: AnyPublisher<ItemStatus, Never> {
        _statusSubject.eraseToAnyPublisher()
    }

    /// Publishes duration changes (seconds).
    public nonisolated var durationPublisher: AnyPublisher<TimeInterval, Never> {
        _durationSubject.eraseToAnyPublisher()
    }

    /// Publishes buffered duration changes (seconds).
    public nonisolated var bufferedDurationPublisher: AnyPublisher<TimeInterval, Never> {
        _bufferedDurationSubject.eraseToAnyPublisher()
    }

    // MARK: - Bitrate preferences

    /// Preferred peak bitrate (0 = no limit).
    public nonisolated var preferredPeakBitrate: Double {
        get { _inner.preferredPeakBitrate() }
        set { _inner.setPreferredPeakBitrate(bitrate: newValue) }
    }

    /// Preferred peak bitrate for expensive networks (0 = no limit).
    public nonisolated var preferredPeakBitrateForExpensiveNetworks: Double {
        get { _inner.preferredPeakBitrateForExpensiveNetworks() }
        set { _inner.setPreferredPeakBitrateForExpensiveNetworks(bitrate: newValue) }
    }

    // MARK: - Internal

    nonisolated let _inner: AudioPlayerItem

    private struct State {
        var status: ItemStatus = .unknown
        var duration: TimeInterval?
        var bufferedDuration: TimeInterval = 0
        var error: KitharaError?
    }

    private let _state = LockedValue(State())
    private let _statusSubject = PassthroughSubject<ItemStatus, Never>()
    private let _durationSubject = PassthroughSubject<TimeInterval, Never>()
    private let _bufferedDurationSubject = PassthroughSubject<TimeInterval, Never>()

    // MARK: - Init

    /// Create a new item for the given URL.
    ///
    /// - Parameters:
    ///   - url: The audio source URL.
    ///   - additionalHeaders: Optional HTTP headers to include in requests.
    public init(url: String, additionalHeaders: [String: String]? = nil) {
        self._inner = AudioPlayerItem(url: url, additionalHeaders: additionalHeaders)
        self.id = _inner.id()
        self.url = url

        let observer = ItemObserverBridge(item: self)
        _inner.setObserver(observer: observer)
    }

    // MARK: - Loading

    /// Asynchronously prepare the underlying resource.
    ///
    /// Must be called before inserting into a ``KitharaPlayer``.
    public func load() async throws {
        do {
            try await _inner.load()
        } catch let ffiError as FfiError {
            throw KitharaError(ffi: ffiError)
        }
    }

    // MARK: - Observer bridge

    fileprivate func handleStatusChanged(_ statusCode: Int32) {
        let newStatus = ItemStatus(statusCode: statusCode)
        _state.withLock { $0.status = newStatus }
        _statusSubject.send(newStatus)
    }

    fileprivate func handleDurationChanged(_ seconds: TimeInterval) {
        _state.withLock { $0.duration = seconds }
        _durationSubject.send(seconds)
    }

    fileprivate func handleBufferedDurationChanged(_ seconds: TimeInterval) {
        _state.withLock { $0.bufferedDuration = seconds }
        _bufferedDurationSubject.send(seconds)
    }

    fileprivate func handleError(code: Int32, message: String) {
        let error: KitharaError = switch code {
        case 1: .notReady
        case 2: .itemFailed(message)
        case 3: .seekFailed(message)
        case 4: .engineNotRunning
        case 5: .invalidArgument(message)
        default: .internal(message)
        }
        _state.withLock { $0.error = error }
    }
}

// MARK: - ItemObserver bridge

private final class ItemObserverBridge: KitharaFFI.ItemObserver, @unchecked Sendable {
    private let _item: LockedValue<KitharaPlayerItem?>

    init(item: KitharaPlayerItem) {
        self._item = LockedValue(item)
    }

    func onDurationChanged(seconds: Double) {
        _item.withLock { $0 }?.handleDurationChanged(seconds)
    }

    func onBufferedDurationChanged(seconds: Double) {
        _item.withLock { $0 }?.handleBufferedDurationChanged(seconds)
    }

    func onStatusChanged(statusCode: Int32) {
        _item.withLock { $0 }?.handleStatusChanged(statusCode)
    }

    func onError(code: Int32, message: String) {
        _item.withLock { $0 }?.handleError(code: code, message: message)
    }
}
