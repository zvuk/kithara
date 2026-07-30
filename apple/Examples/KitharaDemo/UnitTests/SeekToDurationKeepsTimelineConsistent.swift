import AVFAudio
import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    /// Both targets share one player. A second `KitharaPlayer` created while
    /// the previous one is still winding its session down blocks in
    /// `SessionClient::call`, which has nothing to do with this bug.
    @Test("Seeking to the duration keeps timeline values consistent")
    func seekToDurationKeepsTimelineConsistent() async throws {
        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("seek-to-duration-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let audioSession = AVAudioSession.sharedInstance()
        try audioSession.setCategory(.playback)
        try audioSession.setActive(true)

        let player = KitharaPlayer(
            config: .init(store: AssetStore(root: cacheURL.path))
        )
        let item = KitharaPlayerItem(
            url: try TestServerFixture.asset("test.mp3").absoluteString
        )
        let timeline = SeekTimeline()
        let cancellable = player.eventPublisher.sink { event in
            timeline.record(event)
        }
        defer {
            player.stop()
            try? audioSession.setActive(false, options: .notifyOthersOnDeactivation)
            try? FileManager.default.removeItem(at: cacheURL)
            _ = cancellable
        }

        try player.insert(item)
        player.play()
        // Advance first: `item.durationSec` stays 0 until the item's own
        // resource reports one, so the player-side duration is the value that
        // actually settles here.
        try await waitForTimelineFact("fixture playback to advance") {
            player.currentTime > 0.1
        }
        try await waitForTimelineFact("the fixture duration to settle") {
            (player.duration ?? 0) >= 180
        }
        let durationBefore = try #require(
            player.duration,
            "precondition: the fixture duration is unknown"
        )

        for targetKind in [TimelineSeekTarget.justShort, .exact] {
            player.play()
            let target = targetKind.position(for: durationBefore)
            timeline.reset()

            let accepted = await withCheckedContinuation { continuation in
                player.seek(to: target, tolerance: nil) { finished in
                    continuation.resume(returning: finished)
                }
            }
            try #require(
                accepted,
                "precondition: seek to \(targetKind) was rejected"
            )
            try await waitForTimelineFact("a reported outcome for the \(targetKind) seek") {
                let observation = timeline.snapshot()
                return player.currentTime >= target - 0.25 || observation.reachedEnd
            }
            player.pause()

            let durationAfter = try #require(
                player.duration,
                "duration became unknown after the \(targetKind) seek"
            )
            let observation = timeline.snapshot()
            let reportedPositions = observation.positions + [player.currentTime]
            let maxPosition = reportedPositions.max() ?? 0
            let tolerance = 0.01

            #expect(
                maxPosition <= durationAfter + tolerance,
                """
                the \(targetKind) seek reported position \(maxPosition)s \
                beyond duration \(durationAfter)s
                """
            )
            #expect(
                abs(durationAfter - durationBefore) <= tolerance
                    && observation.durations.allSatisfy {
                        abs($0 - durationBefore) <= tolerance
                    },
                """
                duration changed across the \(targetKind) seek; \
                before=\(durationBefore), after=\(durationAfter), \
                published=\(observation.durations)
                """
            )
        }
    }

    private func waitForTimelineFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(45))
        while !condition() {
            guard clock.now < deadline else {
                throw TimelineFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private enum TimelineSeekTarget: CustomStringConvertible {
    case justShort
    case exact

    var description: String {
        switch self {
        case .justShort:
            "duration - 0.05s"
        case .exact:
            "exact-duration"
        }
    }

    func position(for duration: TimeInterval) -> TimeInterval {
        switch self {
        case .justShort:
            duration - 0.05
        case .exact:
            duration
        }
    }
}

private final class SeekTimeline: @unchecked Sendable {
    private let lock = NSLock()
    private var positions: [TimeInterval] = []
    private var durations: [TimeInterval] = []
    private var reachedEnd = false

    func record(_ event: PlayerEvent) {
        lock.lock()
        defer { lock.unlock() }
        switch event {
        case .timeChanged(let seconds):
            positions.append(seconds)
        case .durationChanged(let seconds):
            durations.append(seconds)
        case .itemDidPlayToEnd, .queueEnded:
            reachedEnd = true
        default:
            break
        }
    }

    func reset() {
        lock.lock()
        defer { lock.unlock() }
        positions.removeAll()
        durations.removeAll()
        reachedEnd = false
    }

    func snapshot() -> (
        positions: [TimeInterval],
        durations: [TimeInterval],
        reachedEnd: Bool
    ) {
        lock.lock()
        defer { lock.unlock() }
        return (positions, durations, reachedEnd)
    }
}

private struct TimelineFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
