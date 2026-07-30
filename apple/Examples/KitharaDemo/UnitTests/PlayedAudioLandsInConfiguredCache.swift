import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("Played audio lands in the configured cache")
    func playedAudioLandsInConfiguredCache() async throws {
        let fixtureURL = try TestServerFixture.asset("test.mp3")
        let expectedSize = try await cachedFixtureContentLength(at: fixtureURL)
        try #require(
            expectedSize == 2_994_349,
            """
            precondition: expected the 2,994,349-byte MP3 fixture, \
            got \(expectedSize) bytes
            """
        )

        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("played-audio-cache-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )
        try #require(
            committedCacheFiles(under: cacheURL).isEmpty,
            "precondition: the dedicated cache directory was not empty"
        )

        let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
        let item = KitharaPlayerItem(url: fixtureURL.absoluteString)
        defer {
            player.stop()
            try? FileManager.default.removeItem(at: cacheURL)
        }

        try player.insert(item)
        player.play()
        try await waitForCacheFact("fixture playback to advance") {
            player.currentTime > 0.1
        }
        try await waitForCacheFact("the played MP3 to commit into the configured cache") {
            committedCacheFiles(under: cacheURL).contains { file in
                file.size == expectedSize
            }
        }
        player.pause()

        let files = committedCacheFiles(under: cacheURL)
        #expect(
            files.contains { file in file.size == expectedSize },
            """
            playback advanced, but the configured cache contains no \
            committed \(expectedSize)-byte audio file; files=\(files)
            """
        )
    }

    private func cachedFixtureContentLength(at url: URL) async throws -> UInt64 {
        var request = URLRequest(url: url)
        request.httpMethod = "HEAD"
        let (_, response) = try await URLSession.shared.data(for: request)
        let http = try #require(
            response as? HTTPURLResponse,
            "precondition: fixture HEAD returned a non-HTTP response"
        )
        try #require(
            http.statusCode == 200,
            "precondition: fixture HEAD returned HTTP \(http.statusCode)"
        )
        return UInt64(max(0, http.expectedContentLength))
    }

    private func committedCacheFiles(under root: URL) -> [CommittedCacheFile] {
        guard let files = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.fileSizeKey, .isRegularFileKey]
        ) else {
            return []
        }

        var committed: [CommittedCacheFile] = []
        for case let file as URL in files {
            guard
                let values = try? file.resourceValues(
                    forKeys: [.fileSizeKey, .isRegularFileKey]
                ),
                values.isRegularFile == true,
                let size = values.fileSize,
                size > 0
            else {
                continue
            }
            let isCommittedAsset =
                !file.lastPathComponent.hasSuffix(".tmp")
                && !file.pathComponents.contains("_index")
            if isCommittedAsset {
                committed.append(
                    CommittedCacheFile(
                        path: file.path,
                        size: UInt64(size)
                    )
                )
            }
        }
        return committed
    }

    private func waitForCacheFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(45))
        while !condition() {
            guard clock.now < deadline else {
                throw CacheFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private struct CommittedCacheFile: CustomStringConvertible {
    let path: String
    let size: UInt64

    var description: String {
        "\(path) (\(size) bytes)"
    }
}

private struct CacheFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
