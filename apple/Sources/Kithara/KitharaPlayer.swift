import Combine
import Foundation
import KitharaFFI

/// A queue-based audio player with Combine-driven observation.
///
/// Wraps the low-level ``AudioPlayer`` from the FFI layer and exposes
/// a Swifty API with publishers for all observable state changes.
///
/// ```swift
/// let player = KitharaPlayer()
/// let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
/// try await item.load()
/// try player.insert(item)
/// player.play()
/// ```
public final class KitharaPlayer: @unchecked Sendable {
    private let _inner: AudioPlayer

    // MARK: - Observable state

    private struct State {
        var status: PlayerStatus = .unknown
        var currentTime: TimeInterval = 0
        var duration: TimeInterval?
        var bufferedDuration: TimeInterval = 0
        var error: KitharaError?
    }

    private let _state = LockedValue(State())
    private let _timeSubject = PassthroughSubject<TimeInterval, Never>()
    private let _rateSubject = PassthroughSubject<Float, Never>()
    private let _currentItemSubject = PassthroughSubject<Void, Never>()
    private let _statusSubject = PassthroughSubject<PlayerStatus, Never>()
    private let _errorSubject = PassthroughSubject<KitharaError, Never>()
    private let _durationSubject = PassthroughSubject<TimeInterval, Never>()
    private let _bufferedDurationSubject = PassthroughSubject<TimeInterval, Never>()

    // MARK: - Publishers

    /// Publishes the current playback time in seconds.
    public var timePublisher: AnyPublisher<TimeInterval, Never> {
        _timeSubject.eraseToAnyPublisher()
    }

    /// Publishes playback rate changes.
    public var ratePublisher: AnyPublisher<Float, Never> {
        _rateSubject.eraseToAnyPublisher()
    }

    /// Fires when the current item changes.
    public var currentItemPublisher: AnyPublisher<Void, Never> {
        _currentItemSubject.eraseToAnyPublisher()
    }

    /// Publishes player status changes.
    public var statusPublisher: AnyPublisher<PlayerStatus, Never> {
        _statusSubject.eraseToAnyPublisher()
    }

    /// Publishes errors.
    public var errorPublisher: AnyPublisher<KitharaError, Never> {
        _errorSubject.eraseToAnyPublisher()
    }

    /// Publishes current item duration changes.
    public var durationPublisher: AnyPublisher<TimeInterval, Never> {
        _durationSubject.eraseToAnyPublisher()
    }

    /// Publishes buffered duration changes.
    public var bufferedDurationPublisher: AnyPublisher<TimeInterval, Never> {
        _bufferedDurationSubject.eraseToAnyPublisher()
    }

    // MARK: - Current state accessors

    /// Current player status.
    public var status: PlayerStatus {
        _state.withLock { $0.status }
    }

    /// Current playback time in seconds.
    public var currentTime: TimeInterval {
        _state.withLock { $0.currentTime }
    }

    /// Current item duration in seconds, or `nil` if unknown.
    public var duration: TimeInterval? {
        _state.withLock { $0.duration }
    }

    /// Buffered duration of the current item in seconds.
    public var bufferedDuration: TimeInterval {
        _state.withLock { $0.bufferedDuration }
    }

    /// Last error, or `nil`.
    public var error: KitharaError? {
        _state.withLock { $0.error }
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

    // MARK: - Init

    /// Create a new player instance.
    public init() {
        self._inner = AudioPlayer()

        let bridge = PlayerObserverBridge(player: self)
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
    /// - Parameter seconds: Target time in seconds.
    /// - Throws: ``KitharaError`` if the seek position is invalid.
    public func seek(to seconds: TimeInterval) throws {
        do {
            try _inner.seek(toSeconds: seconds)
        } catch let ffiError as FfiError {
            throw KitharaError(ffi: ffiError)
        }
    }

    // MARK: - Queue management

    private let _items = LockedValue<[KitharaPlayerItem]>([])

    /// The current playback queue.
    public var items: [KitharaPlayerItem] {
        _items.withLock { Array($0) }
    }

    /// Insert an item into the queue.
    ///
    /// - Parameters:
    ///   - item: The item to insert. Must have been ``KitharaPlayerItem/load()``-ed.
    ///   - after: Insert after this item. If `nil`, appends to the end.
    /// - Throws: ``KitharaError`` if `after` is not in the queue or item is not loaded.
    public func insert(_ item: KitharaPlayerItem, after: KitharaPlayerItem? = nil) throws {
        do {
            try _inner.insert(item: item._inner, after: after?._inner)
            _items.withLock { items in
                if let afterItem = after,
                   let idx = items.firstIndex(where: { $0.id == afterItem.id }) {
                    items.insert(item, at: idx + 1)
                } else {
                    items.append(item)
                }
            }
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
            _items.withLock { items in
                items.removeAll { $0.id == item.id }
            }
        } catch let ffiError as FfiError {
            throw KitharaError(ffi: ffiError)
        }
    }

    /// Remove all items from the queue.
    public func removeAllItems() {
        _inner.removeAllItems()
        _items.withLock { $0.removeAll() }
    }

    // MARK: - Observer bridge handlers

    fileprivate func handleTimeChanged(_ seconds: TimeInterval) {
        _state.withLock { $0.currentTime = seconds }
        _timeSubject.send(seconds)
    }

    fileprivate func handleRateChanged(_ rate: Float) {
        _rateSubject.send(rate)
    }

    fileprivate func handleCurrentItemChanged() {
        _currentItemSubject.send()
    }

    fileprivate func handleStatusChanged(_ statusCode: Int32) {
        let newStatus = PlayerStatus(statusCode: statusCode)
        _state.withLock { $0.status = newStatus }
        _statusSubject.send(newStatus)
    }

    fileprivate func handleError(code: Int32, message: String) {
        let error = KitharaError(observerCode: code, message: message)
        _state.withLock { $0.error = error }
        _errorSubject.send(error)
    }

    fileprivate func handleDurationChanged(_ seconds: TimeInterval) {
        _state.withLock { $0.duration = seconds }
        _durationSubject.send(seconds)
    }

    fileprivate func handleBufferedDurationChanged(_ seconds: TimeInterval) {
        _state.withLock { $0.bufferedDuration = seconds }
        _bufferedDurationSubject.send(seconds)
    }
}

// MARK: - PlayerObserver bridge

private final class PlayerObserverBridge: KitharaFFI.PlayerObserver, @unchecked Sendable {
    private let _player: WeakRef<KitharaPlayer>

    init(player: KitharaPlayer) {
        self._player = WeakRef(player)
    }

    func onTimeChanged(seconds: Double) {
        _player.value?.handleTimeChanged(seconds)
    }

    func onRateChanged(rate: Float) {
        _player.value?.handleRateChanged(rate)
    }

    func onCurrentItemChanged() {
        _player.value?.handleCurrentItemChanged()
    }

    func onStatusChanged(statusCode: Int32) {
        _player.value?.handleStatusChanged(statusCode)
    }

    func onError(code: Int32, message: String) {
        _player.value?.handleError(code: code, message: message)
    }

    func onDurationChanged(seconds: Double) {
        _player.value?.handleDurationChanged(seconds)
    }

    func onBufferedDurationChanged(seconds: Double) {
        _player.value?.handleBufferedDurationChanged(seconds)
    }
}
