import Foundation
import KitharaFFI

// MARK: - Player Status

/// Playback readiness state of the player.
public enum PlayerStatus: Sendable {
    case unknown
    case readyToPlay
    case failed
}

// MARK: - Item Status

/// Loading state of a player item.
public enum ItemStatus: Sendable {
    case unknown
    case readyToPlay
    case failed
}

// MARK: - Time Control Status

/// Whether playback is actively producing audio.
public enum TimeControlStatus: Sendable {
    case paused
    case waitingToPlay
    case playing
}

// MARK: - Kithara Error

/// Public error type surfaced by the Kithara Swift layer.
public enum KitharaError: Error, Sendable {
    case notReady
    case itemFailed(String)
    case seekFailed(String)
    case engineNotRunning
    case invalidArgument(String)
    case `internal`(String)
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
