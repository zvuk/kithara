import Foundation
import KitharaFFI

// MARK: - Player Status

/// Playback readiness state of the player.
public enum PlayerStatus: Sendable {
    /// The player has not yet reported a stable readiness state.
    case unknown
    /// The player is ready to start or continue playback.
    case readyToPlay
    /// The player entered a failed state and cannot continue without recovery.
    case failed
}

// MARK: - Item Status

/// Loading state of a player item.
public enum ItemStatus: Sendable {
    /// The item has not yet reported a stable readiness state.
    case unknown
    /// The item is ready for playback.
    case readyToPlay
    /// Item loading or preparation failed.
    case failed
}

// MARK: - Time Control Status

/// Whether playback is actively producing audio.
public enum TimeControlStatus: Sendable {
    /// Playback is paused.
    case paused
    /// The player is waiting to accumulate enough data before playing.
    case waitingToPlay
    /// Playback is in progress and producing audio.
    case playing
}

// MARK: - Kithara Error

/// Public error type surfaced by the Kithara Swift layer.
public enum KitharaError: Error, Sendable {
    /// Operation requires a prepared item or a ready player state.
    case notReady
    /// Item playback or loading failed. The associated value is a human-readable reason.
    case itemFailed(String)
    /// Seek request failed. The associated value is a human-readable reason.
    case seekFailed(String)
    /// The audio engine is not running.
    case engineNotRunning
    /// Supplied argument is invalid. The associated value is a human-readable reason.
    case invalidArgument(String)
    /// Unexpected internal failure propagated from the native layer.
    case `internal`(String)
}

/// Player-level error with optional queue-item attribution.
public enum KitharaPlayerError: Error, Sendable {
    case playback(PlayerError, itemId: TrackId?)
    case command(KitharaError, itemId: TrackId?)

    /// The queue item this error is attributed to, if any.
    public var itemId: TrackId? {
        switch self {
        case .playback(_, let itemId),
             .command(_, let itemId):
            return itemId
        }
    }

    /// The wrapped error — a ``PlayerError`` for `.playback`, a
    /// ``KitharaError`` for `.command`.
    public var underlying: Error {
        switch self {
        case .playback(let error, _):
            return error
        case .command(let error, _):
            return error
        }
    }
}

// MARK: - Player Error (async event stream)

/// Error surfaced on the player / item error publishers
/// (``KitharaPlayer/error`` and ``KitharaPlayerItem/error``).
///
/// Distinct from ``KitharaError`` — that one is thrown synchronously
/// from imperative APIs (`insert`, `remove`, `selectItem`). `PlayerError`
/// is what reactive Combine subscribers see when a problem is observed
/// during playback or item loading.
public enum PlayerError: Error, Sendable, Equatable {
    /// An item failed mid-load or mid-playback. The reason is the
    /// native-side error string.
    case itemFailed(String)
    /// A player-wide error was reported (decoder / pipeline). The
    /// reason is the native-side error string.
    case playerError(String)
    /// Playback stalled (waiting for more data). Emitted by item-level
    /// stalls; surface this in UIs that want to show a buffering chip.
    case stalled
}

// MARK: - ABR

/// HLS variant descriptor.
public struct Variant: Identifiable, Sendable, Equatable {
    /// Variant index in the master playlist.
    public let index: Int
    /// Bandwidth in bits per second.
    public let bandwidthBps: UInt64
    /// Human-readable name, if available.
    public let name: String?

    /// Stable identity for `Identifiable` — the variant ``index``.
    public var id: Int { index }

    init(ffi: FfiVariant) {
        self.index = Int(ffi.index)
        self.bandwidthBps = ffi.bandwidthBps
        self.name = ffi.name
    }
}

/// ABR (Adaptive Bitrate) mode.
public enum AbrMode: Sendable {
    /// Automatic quality selection based on throughput.
    case auto
    /// Fixed to a specific variant.
    case manual(variantIndex: Int)
}

/// Order used when selecting the next item in the queue.
public enum PlaybackOrder: Sendable, Equatable {
    /// Play items in queue order.
    case sequential
    /// Select items in shuffled order.
    case shuffle
}

/// Action taken when the current track reaches its end.
public enum ActionAtItemEnd: Sendable, Equatable {
    case advance
    case pause
    case none
}

/// Gain curve applied during a crossfade.
public enum CrossfadeCurve: Sendable, Equatable {
    case linear
    case equalPower
}

/// Crossfade duration and gain-shaping controls.
public struct CrossfadeSettings: Sendable, Equatable {
    /// Standard crossfade settings used by a new player.
    public static let `default` = CrossfadeSettings(
        validatedDuration: 1,
        curve: .equalPower,
        depth: 1,
        position: 0.5
    )
    /// Duration of the overlap in seconds.
    public let duration: Float
    /// Gain curve used during the overlap.
    public let curve: CrossfadeCurve
    /// Fraction of the full crossfade applied, from zero to one.
    public let depth: Float
    /// Center of the overlap, strictly between zero and one.
    public let position: Float

    /// Create settings after checking finite values and supported ranges.
    public init(
        duration: Float = 1,
        curve: CrossfadeCurve = .equalPower,
        depth: Float = 1,
        position: Float = 0.5
    ) throws {
        guard duration.isFinite, duration >= 0 else {
            throw KitharaError.invalidArgument("crossfade duration must be finite and non-negative")
        }
        guard depth.isFinite, (0...1).contains(depth) else {
            throw KitharaError.invalidArgument("crossfade depth must be finite and in 0...1")
        }
        guard position.isFinite, position > 0, position < 1 else {
            throw KitharaError.invalidArgument("crossfade position must be finite and inside 0...1")
        }
        self.duration = duration
        self.curve = curve
        self.depth = depth
        self.position = position
    }

    private init(
        validatedDuration duration: Float,
        curve: CrossfadeCurve,
        depth: Float,
        position: Float
    ) {
        self.duration = duration
        self.curve = curve
        self.depth = depth
        self.position = position
    }
}

/// How far the whole session output drops under a competing sound.
public enum DuckingMode: Sendable, Equatable {
    /// Full level.
    case off
    /// Lowered to 40%.
    case soft
    /// Lowered to 20%.
    case hard
}

// MARK: - Public type aliases (avoid `import KitharaFFI` in consumer code)

/// Item event from Rust — use with ``KitharaPlayerItem/eventPublisher``.
public typealias ItemEvent = FfiItemEvent

/// Loading/playback status of a track inside the queue.
///
/// Surfaced through `PlayerEvent.trackStatusChanged(itemId:status:)`.
public typealias TrackStatus = FfiTrackStatus

/// Caller-facing track identifier.
///
/// For integrations that already have domain track ids, pass the content id
/// into ``KitharaPlayerItem`` at construction.
/// If omitted, Kithara uses the internally allocated queue id converted to
/// `Int`.
public typealias TrackId = Int

/// Player event dispatched through ``KitharaPlayer/eventPublisher``.
public enum PlayerEvent: Sendable, Equatable {
    case timeChanged(seconds: Double)
    case rateChanged(rate: Float)
    case currentItemChanged(itemId: TrackId?)
    case statusChanged(status: PlayerStatus)
    case timeControlStatusChanged(status: TimeControlStatus)
    case error(String)
    case durationChanged(seconds: Double)
    case bufferedDurationChanged(seconds: Double)
    case volumeChanged(volume: Float)
    case muteChanged(muted: Bool)
    case itemDidPlayToEnd
    case itemDidFail(itemId: TrackId?)
    case trackStatusChanged(itemId: TrackId, status: TrackStatus)
    case queueEnded
    case crossfadeStarted(settings: CrossfadeSettings)
    case crossfadeSettingsChanged(settings: CrossfadeSettings)
    case playbackOrderChanged(order: PlaybackOrder)
    case actionAtItemEndChanged(action: ActionAtItemEnd)
}

// MARK: - Transition

/// Transition style for a track switch.
///
/// Mirrors Apple namespace-struct conventions (e.g. `UIBlurEffect.Style`):
/// - ``none`` — immediate cut. Matches AVQueuePlayer's user-initiated
///   selection idiom (tap an item in a list).
/// - ``crossfade`` — use the player's configured crossfade duration.
///   Typical for Next/Prev buttons and auto-advance at track end.
/// - ``crossfade(settings:)`` — explicit profile override.
public struct Transition: Sendable, Equatable {
    /// Immediate cut.
    public static let none = Transition(ffi: .none)

    /// Use the player's configured crossfade duration.
    public static let crossfade = Transition(ffi: .crossfade)

    /// Use an explicit crossfade profile.
    public static func crossfade(settings: CrossfadeSettings) -> Transition {
        Transition(ffi: .crossfadeWith(settings: settings.ffi))
    }

    let ffi: FfiTransition

    init(ffi: FfiTransition) {
        self.ffi = ffi
    }
}

/// Snapshot of the player state — use with ``KitharaPlayer/snapshot``.
public typealias PlayerSnapshot = FfiPlayerSnapshot

/// Seek completion handler — use with
/// ``KitharaPlayer/seek(to:tolerance:completionHandler:)``.
public typealias SeekCompletionHandler = (Bool) -> Void

// MARK: - Internal conversions

extension TrackId {
    init(ffi id: KitharaFFI.TrackId) {
        guard let converted = TrackId(exactly: id) else {
            preconditionFailure("KitharaFFI TrackId \(id) exceeds Int.max")
        }
        self = converted
    }
}

extension PlayerStatus {
    /// Convert an FFI status from a ``PlayerEvent`` payload into the
    /// Swift-side enum.
    public init(ffi: FfiPlayerStatus) {
        switch ffi {
        case .readyToPlay: self = .readyToPlay
        case .failed: self = .failed
        case .unknown: self = .unknown
        }
    }
}

extension ItemStatus {
    /// Convert an FFI item status from an ``ItemEvent`` payload into
    /// the Swift-side enum.
    public init(ffi: FfiItemStatus) {
        switch ffi {
        case .readyToPlay: self = .readyToPlay
        case .failed: self = .failed
        case .unknown: self = .unknown
        }
    }
}

extension TimeControlStatus {
    /// Convert an FFI time-control status from a ``PlayerEvent``
    /// payload into the Swift-side enum.
    public init(ffi: FfiTimeControlStatus) {
        switch ffi {
        case .paused: self = .paused
        case .waitingToPlay: self = .waitingToPlay
        case .playing: self = .playing
        }
    }
}

extension DuckingMode {
    var ffi: FfiDuckingMode {
        switch self {
        case .off:
            .off
        case .soft:
            .soft
        case .hard:
            .hard
        }
    }
}

extension PlaybackOrder {
    init(ffi: FfiPlaybackOrder) {
        switch ffi {
        case .sequential: self = .sequential
        case .shuffle: self = .shuffle
        case .unknown: preconditionFailure("unknown playback order")
        }
    }
    var ffi: FfiPlaybackOrder { self == .sequential ? .sequential : .shuffle }
}

extension ActionAtItemEnd {
    init(ffi: FfiActionAtItemEnd) {
        switch ffi {
        case .advance: self = .advance
        case .pause: self = .pause
        case .none: self = .none
        case .unknown: preconditionFailure("unknown item-end action")
        }
    }
    var ffi: FfiActionAtItemEnd {
        switch self { case .advance: .advance; case .pause: .pause; case .none: .none }
    }
}

extension CrossfadeCurve {
    init(ffi: FfiCrossfadeCurve) {
        switch ffi {
        case .linear: self = .linear
        case .equalPower: self = .equalPower
        case .unknown: preconditionFailure("unknown crossfade curve")
        }
    }
    var ffi: FfiCrossfadeCurve { self == .linear ? .linear : .equalPower }
}

extension CrossfadeSettings {
    init(ffi: FfiCrossfadeSettings) {
        self.duration = ffi.duration
        self.curve = CrossfadeCurve(ffi: ffi.curve)
        self.depth = ffi.depth
        self.position = ffi.position
    }
    var ffi: FfiCrossfadeSettings {
        FfiCrossfadeSettings(duration: duration, curve: curve.ffi, depth: depth, position: position)
    }
}

extension KitharaError {
    /// Convert an FFI error from a throwing player call into the
    /// Swift-side enum so consumers can pattern-match without
    /// importing `KitharaFFI`.
    public init(ffi: FfiError) {
        switch ffi {
        case .NotInitialized:
            self = .internal("process audio host is not initialized")
        case .InitializationInProgress:
            self = .internal("process audio host initialization is in progress")
        case .AlreadyInitialized:
            self = .internal("process audio host is already initialized")
        case .NotReady:
            self = .notReady
        case let .ItemFailed(reason):
            self = .itemFailed(reason)
        case let .SeekFailed(reason):
            self = .seekFailed(reason)
        case .EngineNotRunning:
            self = .engineNotRunning
        case let .InvalidArgument(reason):
            self = .invalidArgument(reason)
        case let .Internal(message):
            self = .internal(message)
        }
    }
}
