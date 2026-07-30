import AVFAudio
import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("Playback resumes after connectivity returns")
    func playbackResumesAfterConnectivityReturns() async throws {
        try await TestServerFixture.setNetwork(online: true)
        defer {
            Task {
                _ = try? await TestServerFixture.setNetwork(online: true)
            }
        }

        let audioSession = AVAudioSession.sharedInstance()
        try audioSession.setCategory(.playback)
        try audioSession.setActive(true)

        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("connectivity-resume-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
        let item = KitharaPlayerItem(
            url: try TestServerFixture.asset("hls/master.m3u8").absoluteString
        )
        let signals = ResumeSignals()
        let stallCancellable = item.didStall.sink {
            signals.recordStall()
        }
        defer {
            player.stop()
            try? FileManager.default.removeItem(at: cacheURL)
            _ = stallCancellable
        }

        try player.insert(item)
        player.play()
        try await waitForResumeFact("HLS playback to advance before the outage") {
            player.currentTime > 1
        }
        try await waitForResumeFact("the long HLS duration to settle") {
            (player.duration ?? 0) >= 180
        }

        let outageBeganAt = player.currentTime
        try await TestServerFixture.setNetwork(online: false)
        let starvation = await observeStarvation(player)
        try await TestServerFixture.setNetwork(online: true)

        let starvedAt = try #require(
            starvation,
            """
            precondition: playback never stopped advancing while the \
            server was offline; outage began at \(outageBeganAt)s, \
            didStall=\(signals.didStall)
            """
        )
        let resumeTarget = starvedAt + 1
        let resumed = await reachedResumeFact(deadline: .seconds(45)) {
            player.currentTime >= resumeTarget
        }
        #expect(
            resumed,
            """
            connectivity returned after playback starved at \
            \(starvedAt)s, but media time never reached \(resumeTarget)s; \
            current=\(player.currentTime)s
            """
        )
    }

    private func observeStarvation(_ player: KitharaPlayer) async -> TimeInterval? {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(75))
        var lastPosition = player.currentTime
        var stableSince = clock.now

        while clock.now < deadline {
            let position = player.currentTime
            if position > lastPosition + 0.02 {
                lastPosition = position
                stableSince = clock.now
            } else if stableSince.duration(to: clock.now) >= .seconds(2) {
                return position
            }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return nil
    }

    private func reachedResumeFact(
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

    private func waitForResumeFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(45))
        while !condition() {
            guard clock.now < deadline else {
                throw ResumeFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private final class ResumeSignals: @unchecked Sendable {
    private let lock = NSLock()
    private var stalled = false

    var didStall: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stalled
    }

    func recordStall() {
        lock.lock()
        defer { lock.unlock() }
        stalled = true
    }
}

private struct ResumeFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
