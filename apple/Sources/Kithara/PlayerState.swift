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

// MARK: - Time Range

/// A time range expressed in seconds.
public struct TimeRange: Sendable {
    public let start: TimeInterval
    public let duration: TimeInterval

    public init(start: TimeInterval, duration: TimeInterval) {
        self.start = start
        self.duration = duration
    }
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

// MARK: - Internal conversions

extension PlayerStatus {
    init(ffi: FfiPlayerStatus) {
        switch ffi {
        case .readyToPlay: self = .readyToPlay
        case .failed: self = .failed
        case .unknown: self = .unknown
        }
    }

    init(statusCode: Int32) {
        switch statusCode {
        case 1: self = .readyToPlay
        case 2: self = .failed
        default: self = .unknown
        }
    }
}

extension ItemStatus {
    init(ffi: FfiItemStatus) {
        switch ffi {
        case .readyToPlay: self = .readyToPlay
        case .failed: self = .failed
        case .unknown: self = .unknown
        }
    }

    init(statusCode: Int32) {
        switch statusCode {
        case 1: self = .readyToPlay
        case 2: self = .failed
        default: self = .unknown
        }
    }
}

extension TimeControlStatus {
    init(ffi: FfiTimeControlStatus) {
        switch ffi {
        case .paused: self = .paused
        case .waitingToPlay: self = .waitingToPlay
        case .playing: self = .playing
        }
    }
}

extension TimeRange {
    init(ffi: FfiTimeRange) {
        self.init(start: ffi.startSeconds, duration: ffi.durationSeconds)
    }
}

extension KitharaError {
    init(ffi: FfiError) {
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

    /// Map observer callback error codes to ``KitharaError``.
    ///
    /// Codes match Rust `FfiError` variant order:
    /// 1=NotReady, 2=ItemFailed, 3=SeekFailed, 4=EngineNotRunning, 5=InvalidArgument.
    init(observerCode code: Int32, message: String) {
        switch code {
        case 1: self = .notReady
        case 2: self = .itemFailed(message)
        case 3: self = .seekFailed(message)
        case 4: self = .engineNotRunning
        case 5: self = .invalidArgument(message)
        default: self = .internal(message)
        }
    }
}
