import Foundation
@testable import Kithara

enum TestHost {
    private final class State: @unchecked Sendable {
        let lock = NSLock()
        var initialized = false
    }

    private static let state = State()

    static func initialize() throws {
        state.lock.lock()
        defer { state.lock.unlock() }
        guard !state.initialized else { return }
        do {
            try KitharaHost.initialize()
        } catch KitharaHost.InitializationError.alreadyInitialized {
            // Another suite in this test process initialized the one process host.
        }
        state.initialized = true
    }
}
