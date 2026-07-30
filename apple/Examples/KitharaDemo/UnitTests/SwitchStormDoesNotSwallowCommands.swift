import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("A track-switch storm does not swallow later commands")
    func switchStormDoesNotSwallowCommands() async throws {
        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("switch-storm-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let fixture = try await throttledStormFixture()
        let items = (0..<3).map { index in
            KitharaPlayerItem(
                url: fixture.childURL("storm-\(index).mp3").absoluteString
            )
        }
        let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
        let current = SwitchStormCurrentItem()
        let currentCancellable = player.currentItem.sink { item in
            current.record(item)
        }
        defer {
            player.stop()
            try? FileManager.default.removeItem(at: cacheURL)
            _ = currentCancellable
        }

        try player.insert(items[0])
        try player.append(items[1])
        try player.append(items[2])
        try #require(
            player.itemCount == items.count,
            "precondition: the three-item queue was not constructed"
        )

        player.play()
        try await waitForSwitchStormFact("the first throttled track to start") {
            current.matches(items[0]) && player.currentTime > 0.1
        }

        let targets = [
            items[1], items[2], items[0],
            items[2], items[1], items[0],
            items[1], items[2], items[0],
            items[2], items[1], items[0],
        ]
        for (round, target) in targets.enumerated() {
            do {
                try player.selectItem(target, transition: .none)
            } catch {
                throw SwitchStormCommandFailure(
                    "switch \(round) to item \(target.id) was rejected: \(error)"
                )
            }
            let switched = await reachedSwitchStormFact(deadline: .seconds(30)) {
                current.matches(target)
            }
            #expect(
                switched,
                """
                switch \(round) never made item \(target.id) current; \
                observed=\(current.itemID.map(String.init) ?? "nil")
                """
            )
            guard switched else {
                return
            }
        }

        try await waitForSwitchStormFact("the final switched-to track to advance") {
            player.currentTime > 0.1
        }
        try await waitForSwitchStormFact("the final track duration to settle") {
            (player.duration ?? 0) >= 180
        }

        let seekTarget: TimeInterval = 5
        try #require(
            player.currentTime < seekTarget - 1,
            """
            precondition: the final track had already reached \
            \(player.currentTime)s before the \(seekTarget)s seek
            """
        )
        let accepted = await withCheckedContinuation { continuation in
            player.seek(to: seekTarget, tolerance: nil) { finished in
                continuation.resume(returning: finished)
            }
        }
        #expect(
            accepted,
            "seek was rejected after the track-switch storm"
        )
        guard accepted else {
            return
        }

        let landed = await reachedSwitchStormFact(deadline: .seconds(30)) {
            abs(player.currentTime - seekTarget) < 1
        }
        #expect(
            landed,
            """
            seek to \(seekTarget)s did not land after the switch \
            storm; current=\(player.currentTime)s
            """
        )
        guard landed else {
            return
        }

        player.pause()
        let paused = await reachedSwitchStormFact(deadline: .seconds(30)) {
            player.currentRate == 0
        }
        #expect(
            paused,
            """
            pause was swallowed after the switch storm; \
            rate=\(player.currentRate)
            """
        )
        guard paused else {
            return
        }

        let pausedAt = player.currentTime
        player.play()
        let playing = await reachedSwitchStormFact(deadline: .seconds(30)) {
            player.currentRate > 0
        }
        #expect(
            playing,
            """
            play was swallowed after the switch storm; \
            rate=\(player.currentRate)
            """
        )
        guard playing else {
            return
        }

        let carriedOn = await reachedSwitchStormFact(deadline: .seconds(45)) {
            player.currentTime >= pausedAt + 1
        }
        #expect(
            carriedOn,
            """
            playback reported a positive rate after the storm but \
            media time never advanced past \(pausedAt)s
            """
        )
    }

    /// Throttled so the switch storm lands while transfers are still in
    /// flight, which is the state the report describes. The fixture is named
    /// rather than uploaded — a 3 MB body exceeds the server's request limit.
    private func throttledStormFixture() async throws -> TestServerFixture.BehaviorHandle {
        try await TestServerFixture.registerBehavior(
            .init(
                content: .asset(name: "test.mp3"),
                delivery: .throttle(chunk: 4 * 1024, delayMilliseconds: 20)
            )
        )
    }

    private func reachedSwitchStormFact(
        deadline duration: Duration,
        condition: () -> Bool
    ) async -> Bool {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: duration)
        while clock.now < deadline {
            if condition() {
                return true
            }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    private func waitForSwitchStormFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(60))
        while !condition() {
            guard clock.now < deadline else {
                throw SwitchStormFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private final class SwitchStormCurrentItem: @unchecked Sendable {
    private let lock = NSLock()
    private var currentID: Int64?

    var itemID: Int64? {
        lock.lock()
        defer { lock.unlock() }
        return currentID
    }

    func record(_ item: KitharaPlayerItem?) {
        lock.lock()
        defer { lock.unlock() }
        currentID = item?.id
    }

    func matches(_ item: KitharaPlayerItem) -> Bool {
        itemID == item.id
    }
}

private struct SwitchStormCommandFailure: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}

private struct SwitchStormFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
