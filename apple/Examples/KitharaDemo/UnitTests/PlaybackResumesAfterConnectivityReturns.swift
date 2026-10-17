import AVFAudio
import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("Transient HTTP 503 does not fail the current item and playback resumes")
    func transient503DoesNotFailCurrentItemAndPlaybackResumes() async throws {
        try await withRestoredFixtureNetwork {
            let audioSession = AVAudioSession.sharedInstance()
            try audioSession.setCategory(.playback)
            try audioSession.setActive(true)
            defer {
                try? audioSession.setActive(false, options: .notifyOthersOnDeactivation)
            }

            let cacheURL = FileManager.default.temporaryDirectory
                .appendingPathComponent(
                    "connectivity-resume-\(UUID().uuidString)",
                    isDirectory: true
                )
            try FileManager.default.createDirectory(
                at: cacheURL,
                withIntermediateDirectories: true
            )
            defer { try? FileManager.default.removeItem(at: cacheURL) }

            let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
            let item = KitharaPlayerItem(
                url: try await TestServerFixture.pacedHlsMasterURL().absoluteString
            )
            let observation = ConnectivityRecoveryObservation()
            let itemEvents = item.eventPublisher.sink { observation.record($0) }
            let playerEvents = player.eventPublisher.sink { observation.record($0) }
            defer {
                itemEvents.cancel()
                playerEvents.cancel()
                player.stop()
            }

            try player.insert(item)
            player.play()
            try await waitForConnectivityFact("paced HLS playback before the outage") {
                player.currentAudioItem === item
                    && player.currentRate > 0
                    && player.currentTime > 1
                    && (player.duration ?? 0) >= 180
            }

            try await TestServerFixture.setNetwork(.unavailable)
            try await waitForConnectivityFact("public HTTP 503 retry exhaustion") {
                observation.sawRetryExhausted503
                    || observation.terminalFailure != nil
            }
            try #require(
                observation.sawRetryExhausted503,
                "the outage never reached the public HTTP 503 retry-exhausted path"
            )
            try #require(
                observation.terminalFailure == nil,
                "transient HTTP 503 became terminal: \(observation.terminalFailure ?? "none")"
            )

            try await waitForConnectivityFact("an active underrun after HTTP 503") {
                observation.activeUnderrun != nil
                    || observation.terminalFailure != nil
            }
            let underrun = try #require(
                observation.activeUnderrun,
                "playback never entered an active underrun after HTTP 503"
            )
            try #require(
                observation.terminalFailure == nil,
                "the current item failed before connectivity returned"
            )
            try #require(
                player.currentAudioItem === item && player.currentRate > 0,
                "the current item or play intent changed during the outage"
            )

            try await TestServerFixture.setNetwork(.online)
            let resumeTarget = underrun.position + 1
            try await waitForConnectivityFact(
                "the same item to recover after connectivity returned",
                timeout: .seconds(45)
            ) {
                observation.terminalFailure != nil
                    || (
                        observation.didEndUnderrun(eventOrder: underrun.eventOrder)
                            && player.currentAudioItem === item
                            && player.currentRate > 0
                            && player.currentTime >= resumeTarget
                    )
            }

            #expect(
                observation.terminalFailure == nil,
                "connectivity recovery published a terminal failure"
            )
            #expect(
                observation.didEndUnderrun(eventOrder: underrun.eventOrder),
                "the active underrun did not end after connectivity returned"
            )
            #expect(player.currentAudioItem === item, "recovery replaced the current item")
            #expect(player.currentRate > 0, "recovery lost play intent")
            #expect(
                player.currentTime >= resumeTarget,
                "the same item did not advance by one second after recovery"
            )
            if case .failed = player.status {
                Issue.record("player status became failed after transient HTTP 503")
            }
        }
    }

    @Test("Connectivity recovery ends the current underrun, not an earlier one")
    func connectivityRecoveryMatchesTheActiveUnderrun() throws {
        let observation = ConnectivityRecoveryObservation()
        observation.record(ItemEvent.underrunEnded(positionMs: 1_000))
        observation.record(ItemEvent.underrunStarted(positionMs: 1_000))
        let earlier = try #require(observation.activeUnderrun)
        #expect(!observation.didEndUnderrun(eventOrder: earlier.eventOrder))
        observation.record(ItemEvent.underrunEnded(positionMs: 1_000))
        #expect(observation.didEndUnderrun(eventOrder: earlier.eventOrder))
        #expect(observation.activeUnderrun == nil)

        observation.record(ItemEvent.underrunStarted(positionMs: 1_000))
        let current = try #require(observation.activeUnderrun)
        #expect(current.eventOrder > earlier.eventOrder)
        #expect(!observation.didEndUnderrun(eventOrder: current.eventOrder))
        observation.record(ItemEvent.underrunEnded(positionMs: 1_000))
        #expect(observation.didEndUnderrun(eventOrder: current.eventOrder))
        #expect(observation.didEndUnderrun(eventOrder: earlier.eventOrder))
        #expect(observation.activeUnderrun == nil)
    }

    private func withRestoredFixtureNetwork(
        _ operation: () async throws -> Void
    ) async throws {
        try await TestServerFixture.setNetwork(.online)
        do {
            try await operation()
        } catch {
            let operationError = error
            try await TestServerFixture.setNetwork(.online)
            throw operationError
        }
        try await TestServerFixture.setNetwork(.online)
    }

    private func waitForConnectivityFact(
        _ description: String,
        timeout: Duration = .seconds(75),
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: timeout)
        while true {
            try Task.checkCancellation()
            if condition() {
                return
            }
            guard clock.now < deadline else {
                throw ConnectivityFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private struct ActiveConnectivityUnderrun: Sendable {
    let position: TimeInterval
    let eventOrder: UInt64
}

private final class ConnectivityRecoveryObservation: @unchecked Sendable {
    private let lock = NSLock()
    private var retryExhausted503 = false
    private var currentUnderrun: ActiveConnectivityUnderrun?
    private var eventOrder: UInt64 = 0
    private var endedUnderrunOrders: Set<UInt64> = []
    private var failure: String?

    var sawRetryExhausted503: Bool {
        lock.withLock { retryExhausted503 }
    }

    var activeUnderrun: ActiveConnectivityUnderrun? {
        lock.withLock { currentUnderrun }
    }

    var terminalFailure: String? {
        lock.withLock { failure }
    }

    func didEndUnderrun(eventOrder: UInt64) -> Bool {
        lock.withLock { endedUnderrunOrders.contains(eventOrder) }
    }

    func record(_ event: ItemEvent) {
        lock.withLock {
            eventOrder += 1
            switch event {
            case let .downloadRetryExhausted(_, _, _, error):
                if error.contains("503") {
                    retryExhausted503 = true
                }
            case let .underrunStarted(positionMs):
                currentUnderrun = ActiveConnectivityUnderrun(
                    position: TimeInterval(positionMs) / 1_000,
                    eventOrder: eventOrder
                )
            case .underrunEnded:
                if let underrun = currentUnderrun {
                    endedUnderrunOrders.insert(underrun.eventOrder)
                    currentUnderrun = nil
                }
            case .statusChanged(status: .failed):
                recordFailure("item status failed")
            case .didFail:
                recordFailure("item failed")
            case let .trackFailed(reason):
                recordFailure("track failed: \(reason)")
            default:
                break
            }
        }
    }

    func record(_ event: PlayerEvent) {
        lock.withLock {
            switch event {
            case .statusChanged(status: .failed):
                recordFailure("player status failed")
            case let .itemDidFail(itemId):
                recordFailure(
                    itemId.map { "item \($0) failed" } ?? "item failed"
                )
            case let .trackStatusChanged(itemId, status: .failed(reason)):
                recordFailure("item \(itemId) failed: \(reason)")
            default:
                break
            }
        }
    }

    private func recordFailure(_ description: String) {
        if failure == nil {
            failure = description
        }
    }
}

private struct ConnectivityFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
