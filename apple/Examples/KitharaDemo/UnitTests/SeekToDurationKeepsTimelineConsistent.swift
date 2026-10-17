import AVFAudio
import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("Near-duration seek reports causal landing and a bounded timeline")
    func nearDurationSeekReportsCausalLandingAndBoundedTimeline() async throws {
        try await assertNearDurationSeekIsCausalAndBounded(shape: .tagged)
    }

    /// A Xing/Info frame is optional in MPEG audio, and the audiobooks behind
    /// LABA-417 ship without one — their duration lives only in the byte
    /// length. On that shape the reported defect was not a position past
    /// duration but a seek that did nothing at all: with no duration the
    /// decoder cannot turn a target time into a byte offset, so the stream's
    /// cursor never moves and playback runs on from where it already was.
    @Test("Near-duration seek on a headerless CBR MP3 is causal and bounded")
    func headerlessNearDurationSeekReportsCausalLandingAndBoundedTimeline() async throws {
        try await assertNearDurationSeekIsCausalAndBounded(shape: .headerless)
    }

    private func assertNearDurationSeekIsCausalAndBounded(
        shape: TestServerFixture.Mp3Shape
    ) async throws {
        let frameDuration: TimeInterval = 1_152.0 / 44_100.0
        let distanceFromEnd: TimeInterval = 0.1
        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("seek-causal-landing-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let audioSession = AVAudioSession.sharedInstance()
        var audioSessionActivated = false
        defer {
            if audioSessionActivated {
                try? audioSession.setActive(false, options: .notifyOthersOnDeactivation)
            }
            try? FileManager.default.removeItem(at: cacheURL)
        }
        try audioSession.setCategory(.playback)
        try audioSession.setActive(true)
        audioSessionActivated = true

        let player = KitharaPlayer(
            config: .init(store: AssetStore(root: cacheURL.path))
        )
        let item = KitharaPlayerItem(
            url: try await TestServerFixture.signal(
                "signal_mp3_track_sine440_187s.mp3",
                shape: shape
            ).absoluteString
        )
        let completions = SeekCompletionCapture()
        let timeline = SeekBoundaryCapture()
        let itemCancellable = item.eventPublisher.sink { event in
            completions.record(event)
        }
        let playerCancellable = player.eventPublisher.sink { event in
            timeline.record(event)
        }
        let errorCancellable = player.error.sink { error in
            timeline.record(error: error)
        }
        defer {
            itemCancellable.cancel()
            playerCancellable.cancel()
            errorCancellable.cancel()
            player.stop()
        }

        try player.insert(item)
        player.play()
        let startedPlaying = await reachedSeekFact(deadline: .seconds(45)) {
            player.currentTime > 0.1
        }
        try #require(
            startedPlaying,
            "precondition: the \(shape.rawValue) MP3 fixture did not start playing"
        )
        let knowsDuration = await reachedSeekFact(deadline: .seconds(45)) {
            player.duration != nil
        }
        try #require(
            knowsDuration,
            """
            the \(shape.rawValue) MP3 fixture plays but reports no duration, so \
            a seek target cannot be turned into a byte offset and the stream \
            cursor never moves
            """
        )
        let durationBefore = try #require(player.duration)
        try #require(
            durationBefore > distanceFromEnd,
            """
            precondition: duration \(durationBefore)s leaves no room to seek \
            \(distanceFromEnd)s from its end
            """
        )
        let target = durationBefore - distanceFromEnd

        timeline.reset()
        completions.armForNextSeek()
        let accepted = await withCheckedContinuation { continuation in
            player.seek(to: target, tolerance: nil) { finished in
                continuation.resume(returning: finished)
            }
        }
        try #require(accepted, "precondition: the near-duration seek was rejected")

        let publishedCompletion = await reachedSeekFact(deadline: .seconds(15)) {
            completions.snapshot() != nil
        }
        let completion = try #require(
            publishedCompletion ? completions.snapshot() : nil,
            "item.eventPublisher did not emit seekComplete for the issued seek"
        )
        #expect(
            abs(completion.positionSeconds - target) <= frameDuration,
            """
            \(shape.rawValue): seek event \(completion.eventOrder) landed at \
            \(completion.positionSeconds)s for target \(target)s
            """
        )

        let publicReachedTarget = await reachedSeekFact(deadline: .seconds(15)) {
            let observation = timeline.snapshot()
            return observation.failure != nil
                || observation.positions.contains { $0 >= target - frameDuration }
                || player.currentTime >= target - frameDuration
        }
        let targetObservation = timeline.snapshot()
        if let failure = targetObservation.failure {
            throw SeekBoundaryFailure(failure)
        }
        try #require(
            publicReachedTarget,
            "public timeline did not reach the near-duration seek target"
        )

        let reachedEndOrFailed = await reachedSeekFact(deadline: .seconds(15)) {
            let observation = timeline.snapshot()
            return observation.reachedEnd || observation.failure != nil
        }
        let endObservation = timeline.snapshot()
        if let failure = endObservation.failure {
            throw SeekBoundaryFailure(failure)
        }
        try #require(
            reachedEndOrFailed && endObservation.reachedEnd,
            "playback did not reach natural EOF after the near-duration seek"
        )
        try await Task.sleep(nanoseconds: 250_000_000)

        let observation = timeline.snapshot()
        if let failure = observation.failure {
            throw SeekBoundaryFailure(failure)
        }
        let durationAfter = try #require(
            player.duration,
            "duration became unknown after the near-duration seek"
        )
        let publicTime = player.currentTime
        let maximumPosition = (observation.positions + [publicTime]).max() ?? 0
        #expect(
            maximumPosition >= target - frameDuration,
            "public timeline reached only \(maximumPosition)s for target \(target)s"
        )
        #expect(
            maximumPosition <= durationAfter + frameDuration,
            "public timeline reported \(maximumPosition)s beyond duration \(durationAfter)s"
        )
        #expect(
            abs(durationAfter - durationBefore) <= frameDuration
                && observation.durations.allSatisfy {
                    abs($0 - durationBefore) <= frameDuration
                },
            "duration changed across the near-duration seek"
        )
    }

    @Test("Seek completion capture requires a new event after each issued seek")
    func seekCompletionCaptureRequiresAnEventAfterArming() throws {
        let completions = SeekCompletionCapture()
        completions.record(.seekComplete(positionSeconds: 1))
        #expect(completions.snapshot() == nil)

        completions.armForNextSeek()
        #expect(completions.snapshot() == nil)
        completions.record(.seekComplete(positionSeconds: 186.9))
        let first = try #require(completions.snapshot())
        #expect(first.positionSeconds == 186.9)
        completions.record(.seekComplete(positionSeconds: 2))
        #expect(completions.snapshot()?.positionSeconds == first.positionSeconds)

        completions.armForNextSeek()
        #expect(completions.snapshot() == nil)
        completions.record(.seekComplete(positionSeconds: 3))
        let second = try #require(completions.snapshot())
        #expect(second.positionSeconds == 3)
        #expect(second.eventOrder > first.eventOrder)
    }

    private func reachedSeekFact(
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
}

private struct SeekBoundaryFailure: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}

private final class SeekBoundaryCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var positions: [TimeInterval] = []
    private var durations: [TimeInterval] = []
    private var reachedEnd = false
    private var failures: [String] = []

    func record(_ event: PlayerEvent) {
        lock.lock()
        defer { lock.unlock() }
        switch event {
        case let .timeChanged(seconds):
            positions.append(seconds)
        case let .durationChanged(seconds):
            durations.append(seconds)
        case .itemDidPlayToEnd:
            reachedEnd = true
        case let .trackStatusChanged(itemId, status):
            if case let .failed(reason) = status {
                failures.append("item \(itemId) failed: \(reason)")
            }
        case let .itemDidFail(itemId):
            failures.append("item \(itemId.map(String.init) ?? "unknown") failed")
        case let .error(message):
            failures.append(message)
        default:
            break
        }
    }

    func record(error: Error) {
        lock.lock()
        defer { lock.unlock() }
        failures.append(String(describing: error))
    }

    func reset() {
        lock.lock()
        defer { lock.unlock() }
        positions.removeAll()
        durations.removeAll()
        reachedEnd = false
        failures.removeAll()
    }

    func snapshot() -> (
        positions: [TimeInterval],
        durations: [TimeInterval],
        reachedEnd: Bool,
        failure: String?
    ) {
        lock.lock()
        defer { lock.unlock() }
        return (positions, durations, reachedEnd, failures.first)
    }
}

private final class SeekCompletionCapture: @unchecked Sendable {
    private struct Completion {
        let positionSeconds: TimeInterval
        let eventOrder: UInt64
    }

    private let lock = NSLock()
    private var eventOrder: UInt64 = 0
    private var issuedAfterOrder: UInt64 = 0
    private var isArmed = false
    private var completion: Completion?

    func record(_ event: ItemEvent) {
        lock.lock()
        defer { lock.unlock() }

        eventOrder += 1
        guard case let .seekComplete(positionSeconds) = event,
              isArmed,
              completion == nil,
              eventOrder > issuedAfterOrder
        else {
            return
        }
        completion = Completion(positionSeconds: positionSeconds, eventOrder: eventOrder)
    }

    func armForNextSeek() {
        lock.lock()
        defer { lock.unlock() }
        issuedAfterOrder = eventOrder
        completion = nil
        isArmed = true
    }

    func snapshot() -> (positionSeconds: TimeInterval, eventOrder: UInt64)? {
        lock.lock()
        defer { lock.unlock() }
        return completion.map { ($0.positionSeconds, $0.eventOrder) }
    }
}
