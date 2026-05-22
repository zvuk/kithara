import KitharaFFI

/// Severity level for ``Kithara/initLogging(level:)``.
public enum LogLevel: UInt8, Sendable {
    /// Finest-grained per-event traces. High volume.
    case trace = 0
    /// Development-time diagnostic events.
    case debug = 1
    /// High-level lifecycle events. Default.
    case info = 2
    /// Recoverable issues that warrant attention.
    case warn = 3
    /// Failures that need user-visible action.
    case error = 4
    /// Disable logging entirely.
    case off = 255
}

/// Initialize the Rust tracing subscriber so events from the kithara
/// engine (network, decode, playback) are written to stderr. Idempotent.
///
/// On Apple targets, Xcode console captures stderr — call this from app
/// startup to surface engine logs alongside Swift `print()` output.
public func initLogging(level: LogLevel = .info) {
    KitharaFFI.initLogging(level: level.rawValue)
}
