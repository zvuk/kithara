import Foundation

/// Thread-safe weak reference container for observer bridges.
final class WeakRef<T: AnyObject>: @unchecked Sendable {
    private let lock = NSLock()
    private weak var _value: T?

    init(_ value: T) { self._value = value }

    var value: T? {
        lock.lock()
        defer { lock.unlock() }
        return _value
    }
}
