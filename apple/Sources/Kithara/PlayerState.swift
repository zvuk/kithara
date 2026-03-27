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

// MARK: - ABR

/// HLS variant descriptor.
public struct Variant: Identifiable, Sendable, Equatable {
    /// Variant index in the master playlist.
    public let index: Int
    /// Bandwidth in bits per second.
    public let bandwidthBps: UInt64
    /// Human-readable name, if available.
    public let name: String?

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

// MARK: - Public type aliases (avoid `import KitharaFFI` in consumer code)

/// Player event from Rust — use with ``KitharaPlayer/eventPublisher``.
public typealias PlayerEvent = FfiPlayerEvent

/// Item event from Rust — use with ``KitharaPlayerItem/eventPublisher``.
public typealias ItemEvent = FfiItemEvent

/// Snapshot of the player state — use with ``KitharaPlayer/snapshot``.
public typealias PlayerSnapshot = FfiPlayerSnapshot

/// Seek completion callback — use with ``KitharaPlayer/seek(to:callback:)``.
public typealias SeekCallback = KitharaFFI.SeekCallback

// MARK: - Internal conversions

extension PlayerStatus {
    public init(ffi: FfiPlayerStatus) {
        switch ffi {
        case .readyToPlay: self = .readyToPlay
        case .failed: self = .failed
        case .unknown: self = .unknown
        }
    }
}

extension ItemStatus {
    public init(ffi: FfiItemStatus) {
        switch ffi {
        case .readyToPlay: self = .readyToPlay
        case .failed: self = .failed
        case .unknown: self = .unknown
        }
    }
}

extension TimeControlStatus {
    public init(ffi: FfiTimeControlStatus) {
        switch ffi {
        case .paused: self = .paused
        case .waitingToPlay: self = .waitingToPlay
        case .playing: self = .playing
        }
    }
}

extension KitharaError {
    public init(ffi: FfiError) {
        switch ffi {
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
