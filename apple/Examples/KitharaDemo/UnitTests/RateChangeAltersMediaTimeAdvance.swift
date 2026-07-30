import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("A rate change during playback changes how fast media time advances")
    func rateChangeAltersMediaTimeAdvance() async throws {
        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("rate-change-\(UUID().uuidString)", isDirectory: true)
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
        player.playingRate = 1
        player.play()
        try await waitForRateFact("fixture playback to advance at rate 1.0") {
            player.currentTime > 0.1
                && abs(player.currentRate - 1) < 0.05
        }

        let normalAdvance = try await measureMediaTimeAdvance(player)
        try #require(
            normalAdvance > 0,
            "precondition: media time did not advance during the rate-1.0 window"
        )

        // Deliberately not waiting on `currentRate` here: the live rate and the
        // target rate are separate values, and blocking on the live one would
        // turn this trap into a timeout instead of a measurement of the symptom
        // QA reported — playback speed that ignores the requested rate.
        player.playingRate = 2
        try #require(
            abs(player.playingRate - 2) < 0.05,
            """
            precondition: the player did not accept a playing rate of \
            2.0; it reports \(player.playingRate)
            """
        )
        let fastAdvance = try await measureMediaTimeAdvance(player)

        #expect(
            fastAdvance >= normalAdvance * 1.5,
            """
            changing playingRate from 1.0 to 2.0 while playing \
            advanced media time by only \(fastAdvance)s versus \
            \(normalAdvance)s over equal wall-clock windows
            """
        )
    }

    private func measureMediaTimeAdvance(_ player: KitharaPlayer) async throws -> TimeInterval {
        let start = player.currentTime
        try await Task.sleep(nanoseconds: 2_000_000_000)
        return player.currentTime - start
    }

    private func waitForRateFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(30))
        while !condition() {
            guard clock.now < deadline else {
                throw RateFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private struct RateFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
