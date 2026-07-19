import Combine
import Foundation
import KitharaFFI

#if canImport(AVFoundation) && (os(iOS) || os(tvOS) || os(watchOS) || targetEnvironment(macCatalyst))
import AVFoundation
#endif

/// A queue-based audio player with Combine-driven observation.
///
/// Thin wrapper over the Rust `AudioPlayer`. All state lives in Rust —
/// Swift queries it on demand via ``snapshot`` or receives push
/// updates through ``eventPublisher``.
///
/// ```swift
/// let player = KitharaPlayer()
/// let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
/// try player.insert(item)
/// player.play()
/// ```
///
/// `open` for additive subclassing (extra state, methods, protocol
/// conformances). Methods stay `public`, not `open`, so transport behavior
/// cannot be overridden. Subclasses must preserve the `@unchecked Sendable`
/// contract: do not add non-`Sendable` mutable state.
open class KitharaPlayer: KitharaPlayerProtocol, @unchecked Sendable {
    /// Convenience alias for the queue's item type, ``KitharaPlayerItem``.
    public typealias Item = KitharaPlayerItem

    private let _inner: AudioPlayer
    private let _eventSubject = PassthroughSubject<FfiPlayerEvent, Never>()
    private let _commandErrorSubject = PassthroughSubject<KitharaPlayerError, Never>()
    private let _currentItemSubject = CurrentValueSubject<KitharaPlayerItem?, Never>(nil)
    private var _eventCancellable: AnyCancellable?
#if canImport(AVFoundation) && (os(iOS) || os(tvOS) || os(watchOS) || targetEnvironment(macCatalyst))
    private var _audioRouteCancellables = Set<AnyCancellable>()
#endif

    // MARK: - Event stream

    /// Single stream of all player events from Rust.
    public var eventPublisher: AnyPublisher<PlayerEvent, Never> {
        _eventSubject
            .compactMap { [weak self] event in self?.playerEvent(from: event) }
            .eraseToAnyPublisher()
    }

    private func playerEvent(from event: FfiPlayerEvent) -> PlayerEvent? {
        switch event {
        case .timeChanged(let seconds):
            return .timeChanged(seconds: seconds)
        case .rateChanged(let rate):
            return .rateChanged(rate: rate)
        case .currentItemChanged(let itemId):
            return .currentItemChanged(itemId: itemId.flatMap(audioId(for:)))
        case .statusChanged(let status):
            return .statusChanged(status: PlayerStatus(ffi: status))
        case .timeControlStatusChanged(let status):
            return .timeControlStatusChanged(status: TimeControlStatus(ffi: status))
        case .error(let error):
            return .error(error)
        case .durationChanged(let seconds):
            return .durationChanged(seconds: seconds)
        case .bufferedDurationChanged(let seconds):
            return .bufferedDurationChanged(seconds: seconds)
        case .volumeChanged(let volume):
            return .volumeChanged(volume: volume)
        case .muteChanged(let muted):
            return .muteChanged(muted: muted)
        case .itemDidPlayToEnd:
            return .itemDidPlayToEnd
        case .itemDidFail(let itemId):
            return .itemDidFail(itemId: itemId.flatMap(audioId(for:)))
        case .trackStatusChanged(let itemId, let status):
            guard let audioId = audioId(for: itemId) else { return nil }
            return .trackStatusChanged(itemId: audioId, status: status)
        case .queueEnded:
            return .queueEnded
        case .crossfadeStarted(let durationSeconds):
            return .crossfadeStarted(durationSeconds: durationSeconds)
        case .crossfadeDurationChanged(let seconds):
            return .crossfadeDurationChanged(seconds: seconds)
        case .trackAdded,
             .trackRemoved,
             .trackLoadFailed,
             .repeatModeChanged,
             .nextTrackReady,
             .currentItemAdvanced,
             .engineStarted,
             .engineStopped,
             .crossfadeCompleted,
             .crossfadeCancelled,
             .masterVolumeChanged,
             .audioRouteChanged,
             .djBpmDetected,
             .djKeylockChanged,
             .djStretchBackendChanged,
             .transportTempoCommitted,
             .transportPlayStateCommitted,
             .transportSeekCommitted,
             .transportFailed,
             .syncBindingCommitted,
             .syncLockAcquired,
             .syncLockLost,
             .syncRelockCommitted,
             .syncDirectionCommitted,
             .syncUnavailable,
             .assetCommitted,
             .assetFailed,
             .assetEvicted:
            return nil
        }
    }

    private func audioId(for ffiTrackId: KitharaFFI.TrackId) -> TrackId? {
        _knownItems[ffiTrackId]?.audioId
    }

    // MARK: - Discrete publishers (KitharaPlayerProtocol)

    /// Current item publisher. Emits the first queued item before playback
    /// starts, then follows Rust-side playback-current changes. Combine
    /// equivalent of an Rx `rxCurrentAudioItem` bridge.
    public var currentItem: AnyPublisher<KitharaPlayerItem?, Never> {
        _currentItemSubject.eraseToAnyPublisher()
    }

    /// Current playback rate publisher. `0.0` while paused, otherwise
    /// equal to ``playingRate``. Combine equivalent of the iOS
    /// an Rx `rxRate` bridge.
    public var rate: AnyPublisher<Float, Never> {
        _eventSubject
            .compactMap { event -> Float? in
                if case let .rateChanged(rate) = event { return rate }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Live playback-time tick publisher (seconds). Mirrors the iOS
    /// reactive current-time shape so consumers
    /// can drive a slider without polling. The sync ``currentTime``
    /// getter remains for one-shot reads.
    public var currentTimePublisher: AnyPublisher<TimeInterval, Never> {
        _eventSubject
            .compactMap { event -> TimeInterval? in
                if case let .timeChanged(seconds) = event { return seconds }
                return nil
            }
            .eraseToAnyPublisher()
    }

    /// Typed error publisher. Emits runtime playback failures and errors
    /// thrown by imperative queue commands with queue-item attribution when
    /// Kithara can identify the affected item.
    public var contextualError: AnyPublisher<KitharaPlayerError, Never> {
        let playbackErrors = _eventSubject
            .compactMap { [weak self] event in self?.playerError(from: event) }

        return Publishers.Merge(playbackErrors, _commandErrorSubject)
            .eraseToAnyPublisher()
    }

    /// Error publisher. Emits runtime playback failures and errors thrown by
    /// imperative queue commands. Combine equivalent of an Rx `rxError` bridge.
    public var error: AnyPublisher<Error, Never> {
        contextualError
            .map { $0 as Error }
            .eraseToAnyPublisher()
    }

    func playerError(from event: FfiPlayerEvent) -> KitharaPlayerError? {
        switch event {
        case let .error(message):
            return .playback(.playerError(message), itemId: currentAudioItem?.audioId)
        case let .itemDidFail(itemId):
            return .playback(
                .itemFailed(itemId.map(String.init) ?? "<unknown>"),
                itemId: itemId.flatMap(audioId(for:))
            )
        case let .trackLoadFailed(itemId, reason, _):
            return .playback(.itemFailed(reason), itemId: audioId(for: itemId))
        default:
            return nil
        }
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

    /// Current playback time in seconds. `0.0` when no item is loaded.
    /// Mirrors a player `currentTime` sync getter,
    /// resets to `0` on item removal).
    public var currentTime: TimeInterval {
        _inner.currentTime()
    }

    /// Current item duration in seconds, or `nil` if unknown.
    public var duration: TimeInterval? {
        _inner.snapshot().duration
    }

    /// Current queue item visible to iOS-style clients, or `nil` when
    /// the queue is empty.
    public var currentAudioItem: KitharaPlayerItem? {
        _currentItemSubject.value
    }

    // MARK: - Rate

    /// Synchronous one-shot read of the live playback rate (`0.0` while
    /// paused). Counterpart of the publisher ``rate``.
    public var currentRate: Float {
        _inner.rate()
    }

    /// Target playback speed used by ``play()``. Mirrors the iOS
    /// `playingRate`: while playing, ``rate`` equals
    /// this value; on pause, ``rate`` falls to `0`.
    public var playingRate: Float {
        get { _inner.playingRate() }
        set { _inner.setPlayingRate(rate: newValue) }
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

    /// A single DRM rule: a key processor bound to one or more domain
    /// patterns (exact `"example.com"`, wildcard subdomain
    /// `"*.example.com"`, or match-any `"*"`), plus optional headers /
    /// query params sent with matching key requests, and an optional
    /// per-rule salt forwarded to ``KeyProcessor/processKey(_:salt:)``.
    public struct KeyRule: Sendable {
        /// Cipher applied to the encrypted key bytes on every fetch.
        public let processor: KeyProcessor
        /// Hosts this rule applies to. Use `["*"]` to match any host.
        public let domains: [String]
        /// Extra HTTP headers attached to every key request that hits
        /// one of the configured domains.
        public let headers: [String: String]?
        /// Extra query-string params attached to every key request.
        public let queryParams: [String: String]?
        /// Optional salt forwarded to
        /// ``KeyProcessor/processKey(_:salt:)`` on each decrypt.
        public let salt: String?

        /// Construct a DRM rule. `processor` is required; everything
        /// else may be left at default.
        public init(
            processor: KeyProcessor,
            domains: [String],
            headers: [String: String]? = nil,
            queryParams: [String: String]? = nil,
            salt: String? = nil
        ) {
            self.processor = processor
            self.domains = domains
            self.headers = headers
            self.queryParams = queryParams
            self.salt = salt
        }
    }

    /// Configuration for player creation.
    public struct Config: Sendable {
        /// Number of EQ bands (log-spaced). Default: 10.
        public var eqBandCount: Int
        /// Domain-scoped DRM rules. Evaluated in order; first match wins.
        /// Wildcard `"*"` rules must come last — they mask any rule
        /// registered after them.
        public var keyRules: [KeyRule]
        /// Shared Rust-owned asset store used by this player.
        public var store: AssetStore

        /// Construct a player config. All parameters have sensible
        /// defaults; pass DRM `keyRules` for encrypted streams.
        public init(
            eqBandCount: Int = 10,
            keyRules: [KeyRule] = [],
            store: AssetStore = AssetStore()
        ) {
            self.eqBandCount = eqBandCount
            self.keyRules = keyRules
            self.store = store
        }
    }

    /// Create a new player instance.
    public init(config: Config = Config()) {
        let ffiRules = config.keyRules.map { rule -> FfiKeyRule in
            FfiKeyRule(
                processor: KeyProcessorBridge(processor: rule.processor),
                headers: rule.headers,
                queryParams: rule.queryParams,
                salt: rule.salt,
                domains: rule.domains
            )
        }
        let ffiConfig = FfiPlayerConfig(
            keyOptions: FfiKeyOptions(rules: ffiRules),
            store: config.store.inner,
            eqBandCount: UInt32(config.eqBandCount)
        )
        self._inner = AudioPlayer(config: ffiConfig)

        let bridge = PlayerObserverBridge(subject: _eventSubject)
        _inner.setObserver(observer: bridge)
        _eventCancellable = _eventSubject.sink { [weak self] event in
            self?.syncCurrentItem(with: event)
        }
        bindPlatformAudioRouteInvalidation()
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

    /// Notify Kithara that the platform audio route changed.
    public func notifyAudioRouteChanged(reason: String) {
        do {
            try _inner.notifyAudioRouteChanged(reason: reason)
        } catch {
            _eventSubject.send(.error(error: String(describing: error)))
        }
    }

#if canImport(AVFoundation) && (os(iOS) || os(tvOS) || os(watchOS) || targetEnvironment(macCatalyst))
    private func bindPlatformAudioRouteInvalidation() {
        NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)
            .sink { [weak self] _ in
                self?.notifyAudioRouteChanged(reason: "AVAudioSession.routeChange")
            }
            .store(in: &_audioRouteCancellables)

        NotificationCenter.default.publisher(for: AVAudioSession.mediaServicesWereResetNotification)
            .sink { [weak self] _ in
                self?.notifyAudioRouteChanged(reason: "AVAudioSession.mediaServicesWereReset")
            }
            .store(in: &_audioRouteCancellables)
    }
#else
    private func bindPlatformAudioRouteInvalidation() {}
#endif

    /// Seek to a position in the current item.
    ///
    /// - Parameters:
    ///   - seconds: Target time in seconds.
    ///   - tolerance: Optional landing-tolerance in seconds. Currently
    ///     advisory — the engine uses its own seek heuristics; passing
    ///     `nil` preserves legacy behaviour and reserves the slot for
    ///     future precise-seek wiring.
    ///   - completionHandler: Invoked with `true` if the seek was
    ///     accepted, `false` otherwise.
    public func seek(
        to seconds: TimeInterval,
        tolerance: TimeInterval? = nil,
        completionHandler: @escaping SeekCompletionHandler
    ) {
        _inner.seek(
            toSeconds: seconds,
            tolerance: tolerance,
            callback: ClosureSeekCallback(completionHandler)
        )
    }

    // MARK: - Queue management (delegated to Rust)

    /// Items inserted via ``insert(_:after:)``, preserving Swift identity.
    private var _knownItems: [KitharaFFI.TrackId: KitharaPlayerItem] = [:]
    private var _knownRepresentations: [KitharaFFI.TrackId: AnyObject] = [:]

    private func syncCurrentItem(with event: FfiPlayerEvent) {
        guard case let .currentItemChanged(itemId) = event else { return }
        publishCurrentItem(itemId.flatMap { _knownItems[$0] })
    }

    private func publishCurrentItem(_ item: KitharaPlayerItem?) {
        if _currentItemSubject.value?.ffiTrackId != item?.ffiTrackId {
            _currentItemSubject.send(item)
        }
    }

    private func publishQueueHeadAsCurrent() {
        publishCurrentItem(items().first)
    }

    private func publishCommandError(_ error: KitharaError, itemId: TrackId?) {
        _commandErrorSubject.send(.command(error, itemId: itemId))
    }

    /// The current playback queue.
    ///
    /// Returns the same Swift instances that were passed to ``insert(_:after:)``,
    /// preserving identity and active event publishers. Mirrors the iOS
    /// Player queue as a function, not a property.
    public func items() -> [KitharaPlayerItem] {
        let ffiItems = _inner.items()
        return ffiItems.compactMap { ffiItem in
            let id = ffiItem.queueId()
            return _knownItems[id]
        }
    }

    /// Current item represented as a caller-owned wrapper type, when the item
    /// was inserted through ``insert(_:representing:after:)``.
    public func currentItemRepresentation<T: AnyObject>(as type: T.Type = T.self) -> T? {
        currentAudioItem.flatMap { _knownRepresentations[$0.ffiTrackId] as? T }
    }

    /// Current item representation publisher. Emits the caller-owned wrapper
    /// object registered by ``insert(_:representing:after:)``.
    public func currentItemRepresentationPublisher<T: AnyObject>(
        as type: T.Type = T.self
    ) -> AnyPublisher<T?, Never> {
        _currentItemSubject
            .map { [weak self] item -> T? in
                guard let self, let item else { return nil }
                return self._knownRepresentations[item.ffiTrackId] as? T
            }
            .eraseToAnyPublisher()
    }

    /// Queue represented as caller-owned wrapper objects, preserving Rust queue
    /// order.
    public func itemRepresentations<T: AnyObject>(as type: T.Type = T.self) -> [T] {
        items().compactMap { _knownRepresentations[$0.ffiTrackId] as? T }
    }

    /// Insert an item into the queue.
    ///
    /// - Parameters:
    ///   - item: The item to insert. May be loaded or not yet loaded (auto-load).
    ///   - after: Insert after this item. If `nil`, the item is placed
    ///     at the **head** (position 0), per the iOS
    ///     the legacy iOS insert-at-head contract. Use
    ///     ``append(_:)`` for AVQueuePlayer-style append.
    /// - Throws: ``KitharaError`` if `after` is not in the queue.
    public func insert(_ item: KitharaPlayerItem, after: KitharaPlayerItem? = nil) throws {
        let wasEmpty = itemCount == 0
        _knownItems[item.ffiTrackId] = item
        do {
            try _inner.insert(item: item._inner, after: after?._inner)
            if wasEmpty {
                publishCurrentItem(item)
            }
        } catch let ffiError as FfiError {
            _knownItems.removeValue(forKey: item.ffiTrackId)
            let error = KitharaError(ffi: ffiError)
            publishCommandError(error, itemId: item.audioId)
            throw error
        }
    }

    /// Insert an item while registering a caller-owned wrapper object for UI /
    /// legacy protocol surfaces. Playback still uses `item`; representation
    /// accessors return `representation`.
    public func insert<T: AnyObject>(
        _ item: KitharaPlayerItem,
        representing representation: T,
        after: KitharaPlayerItem? = nil
    ) throws {
        _knownRepresentations[item.ffiTrackId] = representation
        do {
            try insert(item, after: after)
        } catch {
            _knownRepresentations.removeValue(forKey: item.ffiTrackId)
            throw error
        }
    }

    /// Append an item to the tail of the queue (AVQueuePlayer-style).
    /// Counterpart of ``insert(_:after:)``, which inserts at the head
    /// when `after == nil` per the iOS protocol contract.
    public func append(_ item: KitharaPlayerItem) throws {
        let wasEmpty = itemCount == 0
        _knownItems[item.ffiTrackId] = item
        do {
            try _inner.append(item: item._inner)
            if wasEmpty {
                publishCurrentItem(item)
            }
        } catch let ffiError as FfiError {
            _knownItems.removeValue(forKey: item.ffiTrackId)
            let error = KitharaError(ffi: ffiError)
            publishCommandError(error, itemId: item.audioId)
            throw error
        }
    }

    /// Remove an item from the queue.
    ///
    /// - Parameter item: The item to remove.
    /// - Throws: ``KitharaError`` if the item is not in the queue.
    public func remove(_ item: KitharaPlayerItem) throws {
        do {
            try _inner.remove(item: item._inner)
            _knownItems.removeValue(forKey: item.ffiTrackId)
            _knownRepresentations.removeValue(forKey: item.ffiTrackId)
            if _currentItemSubject.value?.ffiTrackId == item.ffiTrackId {
                publishQueueHeadAsCurrent()
            }
        } catch let ffiError as FfiError {
            let error = KitharaError(ffi: ffiError)
            publishCommandError(error, itemId: item.audioId)
            throw error
        }
    }

    /// Remove all items from the queue.
    public func removeAllItems() {
        _inner.removeAllItems()
        _knownItems.removeAll()
        _knownRepresentations.removeAll()
        publishCurrentItem(nil)
    }

    /// Number of items currently in the queue.
    public var itemCount: Int {
        Int(_inner.itemCount())
    }

    /// Replace the item at `index` with a freshly-loaded one.
    ///
    /// Use before ``selectItem(at:transition:)`` to re-play a track whose
    /// resource was consumed by a prior playback. The replacement item
    /// must already have a loaded resource (``KitharaPlayerItem/load()``
    /// finished).
    public func replaceItem(at index: Int, with item: KitharaPlayerItem) throws {
        let oldItems = items()
        let replacesCurrent = oldItems.indices.contains(index)
            && oldItems[index].ffiTrackId == _currentItemSubject.value?.ffiTrackId
        let oldItemId = oldItems.indices.contains(index) ? oldItems[index].ffiTrackId : nil
        let oldRepresentation = oldItemId.flatMap { _knownRepresentations[$0] }
        _knownItems[item.ffiTrackId] = item
        if let oldItemId {
            _knownRepresentations.removeValue(forKey: oldItemId)
        }
        do {
            try _inner.replaceItem(index: UInt32(index), item: item._inner)
            if replacesCurrent {
                publishCurrentItem(item)
            }
        } catch let ffiError as FfiError {
            _knownItems.removeValue(forKey: item.ffiTrackId)
            if let oldItemId, let oldRepresentation {
                _knownRepresentations[oldItemId] = oldRepresentation
            }
            let error = KitharaError(ffi: ffiError)
            publishCommandError(error, itemId: item.audioId)
            throw error
        }
    }

    /// Select an item at the given queue index.
    ///
    /// - Parameters:
    ///   - index: Queue index (0-based).
    ///   - transition: How the switch plays — `.none` for an immediate
    ///     cut (AVQueuePlayer user-initiated-selection idiom — default),
    ///     `.crossfade` to use the player's configured duration.
    /// - Throws: ``KitharaError`` if the index is out of range or the item
    ///   is not yet inserted into the engine.
    public func selectItem(at index: Int, transition: Transition = .none) throws {
        let snapshot = items()
        let itemId = snapshot.indices.contains(index) ? snapshot[index].audioId : nil
        do {
            try _inner.selectItem(index: UInt32(index), transition: transition.ffi)
        } catch let ffiError as FfiError {
            let error = KitharaError(ffi: ffiError)
            publishCommandError(error, itemId: itemId)
            throw error
        }
    }

    /// Select an item by identity (AVQueuePlayer-style).
    ///
    /// Resolves the item's current index via ``items()`` and delegates to
    /// ``selectItem(at:transition:)``. Race-free against concurrent
    /// `insert`/`remove` that would shift indices.
    ///
    /// - Parameters:
    ///   - item: The item to select. Must currently be in the queue.
    ///   - transition: `.none` by default (immediate cut); pass
    ///     `.crossfade` for Next/Prev button UX.
    /// - Throws: ``KitharaError/invalidArgument(_:)`` if the item is not in
    ///   the queue, or whatever ``selectItem(at:transition:)`` throws.
    public func selectItem(_ item: KitharaPlayerItem, transition: Transition = .none) throws {
        let snapshot = items()
        guard let index = snapshot.firstIndex(where: { $0.ffiTrackId == item.ffiTrackId }) else {
            let error = KitharaError.invalidArgument("item \(item.id) not in queue")
            publishCommandError(error, itemId: item.audioId)
            throw error
        }
        try selectItem(at: index, transition: transition)
    }

    /// Crossfade duration in seconds applied on item transitions.
    public var crossfadeDuration: Float {
        get { _inner.crossfadeDuration() }
        set { _inner.setCrossfadeDuration(seconds: newValue) }
    }

    // MARK: - Queue navigation

    /// Skip to the next item in the queue. No-op if already on the
    /// last item or the queue is empty. Mirrors AVQueuePlayer's
    /// `advanceToNextItem`.
    public func advanceToNextItem() {
        _inner.advanceToNextItem()
    }

    /// Stop playback, clear the queue, and reset the current-item
    /// slot. Mirrors `AVPlayer.stop` semantics — after `stop()`, the
    /// player is ready to accept a fresh queue via ``insert(_:after:)``.
    public func stop() {
        _inner.stop()
        _knownItems.removeAll()
        publishCurrentItem(nil)
    }

    // MARK: - Network / DRM hooks

    /// Configure the auth token sent on every player HTTP request.
    /// Pass an empty string to clear.
    public func setupNetwork(authToken: String) {
        _inner.setupNetwork(authToken: authToken)
    }

    /// Per-network bitrate ceilings (bits/sec). Pass `0` for either
    /// argument to lift that limit; with both zero the ABR considers
    /// every variant.
    public func updatePeakBitrate(wifi: Double, cellular: Double) {
        _inner.updatePeakBitrate(wifiBps: wifi, cellularBps: cellular)
    }

    /// Register a runtime DRM key decryptor on every host (default
    /// `"*"` wildcard). The closure receives the encrypted key bytes
    /// plus the player-generated salt that was attached to outgoing
    /// requests under `X-Encrypted-Key`. Returning `nil` from the
    /// closure preserves the input ciphertext unchanged.
    public func setupHlsAes(keyDecryptor: @escaping (Data, String) -> Data?) {
        let bridge = ClosureKeyProcessorBridge(decrypt: keyDecryptor)
        _inner.setupHlsAes(processor: bridge)
    }

    /// Register a runtime DRM key processor with explicit rule control
    /// (custom domains, headers, query params, salt). The rule's salt,
    /// if any, is mirrored into the player-wide HTTP header set so it
    /// accompanies every outgoing request matching the rule's domains.
    public func setupHlsAes(rule: KeyRule) {
        let ffiRule = FfiKeyRule(
            processor: KeyProcessorBridge(processor: rule.processor),
            headers: rule.headers,
            queryParams: rule.queryParams,
            salt: rule.salt,
            domains: rule.domains
        )
        _inner.setupHlsAesWithRule(rule: ffiRule)
    }

}

private final class ClosureSeekCallback: KitharaFFI.SeekCallback, @unchecked Sendable {
    private let completionHandler: SeekCompletionHandler

    init(_ completionHandler: @escaping SeekCompletionHandler) {
        self.completionHandler = completionHandler
    }

    func onComplete(finished: Bool) {
        completionHandler(finished)
    }
}

// MARK: - Key processor

/// Callback for processing (decrypting) HLS encryption keys.
///
/// Implement this protocol to provide custom key decryption logic.
/// The player calls ``processKey(_:salt:)`` for each key fetched from
/// the server, passing the salt the player generated and attached to
/// outgoing requests under `X-Encrypted-Key`.
public protocol KeyProcessor: Sendable {
    /// Process (decrypt) raw key bytes received from the server.
    ///
    /// - Parameters:
    ///   - key: Encrypted key bytes.
    ///   - salt: Player-generated salt accompanying the key request
    ///     (the value mirrored into the `X-Encrypted-Key` HTTP
    ///     header). Implementations that derive the cipher per-session
    ///     can rebuild it from this; static-cipher implementations may
    ///     ignore the argument.
    /// - Returns: Decrypted key bytes.
    func processKey(_ key: Data, salt: String) -> Data
}

private final class KeyProcessorBridge: KitharaFFI.FfiKeyProcessor, @unchecked Sendable {
    private let processor: KeyProcessor

    init(processor: KeyProcessor) {
        self.processor = processor
    }

    func processKey(key: Data, salt: String) -> Data {
        processor.processKey(key, salt: salt)
    }
}

/// Closure-based bridge for ``KitharaPlayer/setupHlsAes(keyDecryptor:)``.
private final class ClosureKeyProcessorBridge: KitharaFFI.FfiKeyProcessor, @unchecked Sendable {
    private let decrypt: (Data, String) -> Data?

    init(decrypt: @escaping (Data, String) -> Data?) {
        self.decrypt = decrypt
    }

    func processKey(key: Data, salt: String) -> Data {
        decrypt(key, salt) ?? key
    }
}

// MARK: - PlayerObserver bridge (single on_event callback)

private final class PlayerObserverBridge: KitharaFFI.PlayerObserver, @unchecked Sendable {
    private let subject: PassthroughSubject<FfiPlayerEvent, Never>

    init(subject: PassthroughSubject<FfiPlayerEvent, Never>) {
        self.subject = subject
    }

    func onEvent(event: FfiPlayerEvent) {
        subject.send(event)
    }
}
