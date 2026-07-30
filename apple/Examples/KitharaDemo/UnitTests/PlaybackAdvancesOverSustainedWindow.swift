import AVFAudio
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    /// The lane's control, mirroring `lane_smoke.rs` on the Rust side: it
    /// distinguishes "the framework cannot play at all" from "this trap's bug
    /// reproduced". Several traps assert that playback keeps advancing after
    /// some event; none of those verdicts mean anything while this one is red.
    @Test("Playback keeps advancing over a sustained window")
    func playbackAdvancesOverSustainedWindow() async throws {
        let audioSession = AVAudioSession.sharedInstance()
        try audioSession.setCategory(.playback)
        try audioSession.setActive(true)

        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("sustained-playback-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
        let item = KitharaPlayerItem(
            url: try TestServerFixture.asset("test.mp3").absoluteString
        )
        defer {
            player.stop()
            try? FileManager.default.removeItem(at: cacheURL)
        }

        try player.insert(item)
        player.play()
        try await waitForControlFact("playback to start") {
            player.currentTime > 0.1
        }

        // Media time against wall-clock time is the one place a stopwatch is
        // the subject rather than a proxy: the property under test is that the
        // two keep pace. Sampling is coarse enough that scheduler jitter
        // cannot fake it.
        let startedAt = ContinuousClock().now
        let startPosition = player.currentTime
        let reachedTarget = await reachedControlFact(deadline: .seconds(20)) {
            player.currentTime - startPosition >= 5
        }
        let elapsed = ContinuousClock().now - startedAt

        #expect(
            reachedTarget,
            """
            playback stalled — media time moved from \
            \(startPosition)s to \(player.currentTime)s over \(elapsed) of \
            wall clock. Every "playback did not advance" verdict in this lane \
            is unattributable while this is red.
            """
        )
    }

    private func reachedControlFact(
        deadline duration: Duration,
        condition: () -> Bool
    ) async -> Bool {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: duration)
        while clock.now < deadline {
            if condition() {
                return true
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        return condition()
    }

    private func waitForControlFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(30))
        while !condition() {
            guard clock.now < deadline else {
                throw SustainedPlaybackTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private struct SustainedPlaybackTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
