import KitharaFFI

/// Severity level for ``Kithara/initLogging(level:)``.
public enum LogLevel: UInt8, Sendable {
    case trace = 0
    case debug = 1
    case info = 2
    case warn = 3
    case error = 4
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
