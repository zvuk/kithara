import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("Repeat mode is writable, readable, and observable")
    func repeatModeIsWritableReadableObservable() async throws {
        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("repeat-mode-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
        let events = RepeatModeEvents()
        let cancellable = player.eventPublisher.sink { event in
            if case let .repeatModeChanged(mode) = event {
                events.record(mode)
            }
        }
        defer {
            player.stop()
            try? FileManager.default.removeItem(at: cacheURL)
            _ = cancellable
        }

        player.repeatMode = .one
        try await waitForRepeatModeFact("repeatModeChanged(.one)") {
            events.contains(.one)
        }
        try await waitForRepeatModeFact("repeatMode getter to report .one") {
            player.repeatMode == .one
        }

        #expect(events.contains(.one))
        #expect(player.repeatMode == .one)
    }

    private func waitForRepeatModeFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(5))
        while !condition() {
            guard clock.now < deadline else {
                throw RepeatModeFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private final class RepeatModeEvents: @unchecked Sendable {
    private let lock = NSLock()
    private var modes: [RepeatMode] = []

    func record(_ mode: RepeatMode) {
        lock.lock()
        defer { lock.unlock() }
        modes.append(mode)
    }

    func contains(_ mode: RepeatMode) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return modes.contains(mode)
    }
}

private struct RepeatModeFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
