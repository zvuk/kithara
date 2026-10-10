import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("Default iOS playback stores exact audio bytes under Documents/Files/Kithara")
    func defaultIOSPlaybackUsesApplicationFilesCache() async throws {
        let fixture = try await TestServerFixture.registerBehavior(
            .init(
                content: .signal(name: "signal_mp3_track_sine440_187s.mp3", shape: .tagged),
                delivery: .normal
            )
        )
        let fixtureURL = fixture.childURL("laba-413-\(UUID().uuidString).mp3")
        let (fixtureBytes, response) = try await URLSession.shared.data(from: fixtureURL)
        let http = try #require(
            response as? HTTPURLResponse,
            "precondition: the unique fixture route returned a non-HTTP response"
        )
        try #require(
            http.statusCode == 200,
            "precondition: the unique fixture route returned HTTP \(http.statusCode)"
        )
        try #require(
            Int64(fixtureBytes.count) == http.expectedContentLength,
            """
            precondition: the fixture route delivered \(fixtureBytes.count) of the \
            \(http.expectedContentLength) bytes it advertised
            """
        )

        let fileManager = FileManager.default
        let documentsURL = try fileManager.url(
            for: .documentDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        let applicationFilesURL = documentsURL
            .appendingPathComponent("Files", isDirectory: true)
        let cacheURL = applicationFilesURL
            .appendingPathComponent("Kithara", isDirectory: true)
        let baselineCachePaths = Set(
            try defaultCacheEntries(under: cacheURL).map(\.standardizedFileURL.path)
        )
        let baselineLegacyPaths = Set(
            try defaultCacheEntries(under: applicationFilesURL)
                .filter { !defaultIsInside($0, root: cacheURL) }
                .map(\.standardizedFileURL.path)
        )
        defer {
            do {
                try defaultRemoveCacheDelta(under: cacheURL, baselinePaths: baselineCachePaths)
            } catch {
                Issue.record("failed to remove the LABA-413 cache delta: \(error)")
            }
        }

        let committedFiles = try await defaultPlayWithDefaultCache(
            fixtureURL: fixtureURL,
            expectedSize: fixtureBytes.count,
            cacheURL: cacheURL,
            baselinePaths: baselineCachePaths
        )
        let containsExactFixture = try committedFiles.contains { file in
            try Data(contentsOf: file) == fixtureBytes
        }
        #expect(
            containsExactFixture,
            "the default player did not commit the exact fixture bytes under \(cacheURL.path)"
        )

        let cacheValues = try cacheURL.resourceValues(forKeys: [.isExcludedFromBackupKey])
        #expect(
            cacheValues.isExcludedFromBackup == true,
            "the default Kithara cache is included in iOS backups"
        )

        let legacyDelta = try defaultCacheEntries(under: applicationFilesURL)
            .filter { !defaultIsInside($0, root: cacheURL) }
            .filter { !baselineLegacyPaths.contains($0.standardizedFileURL.path) }
        #expect(
            legacyDelta.isEmpty,
            "Kithara wrote into the legacy Files cache instead of its Kithara child: \(legacyDelta)"
        )
    }

    private func defaultPlayWithDefaultCache(
        fixtureURL: URL,
        expectedSize: Int,
        cacheURL: URL,
        baselinePaths: Set<String>
    ) async throws -> [URL] {
        let player = KitharaPlayer()
        let item = KitharaPlayerItem(url: fixtureURL.absoluteString)
        defer {
            player.stop()
        }

        try player.insert(item)
        player.play()
        try await defaultWaitForCacheFact("fixture playback to advance") {
            player.currentTime > 0.1
        }
        try await defaultWaitForCacheFact("the exact-size MP3 to commit into the default cache") {
            try defaultCommittedCacheFiles(under: cacheURL, excluding: baselinePaths)
                .contains { file in
                    try file.resourceValues(forKeys: [.fileSizeKey]).fileSize == expectedSize
                }
        }
        return try defaultCommittedCacheFiles(under: cacheURL, excluding: baselinePaths)
    }

    private func defaultCommittedCacheFiles(
        under root: URL,
        excluding baselinePaths: Set<String>
    ) throws -> [URL] {
        var committed: [URL] = []
        for file in try defaultCacheEntries(under: root) {
            let path = file.standardizedFileURL.path
            guard !baselinePaths.contains(path) else {
                continue
            }
            let values = try file.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
            guard
                values.isRegularFile == true,
                let size = values.fileSize,
                size > 0,
                !file.lastPathComponent.hasSuffix(".tmp"),
                !file.pathComponents.contains("_index")
            else {
                continue
            }
            committed.append(file)
        }
        return committed
    }

    private func defaultCacheEntries(under root: URL) throws -> [URL] {
        guard FileManager.default.fileExists(atPath: root.path) else {
            return []
        }
        return try FileManager.default.subpathsOfDirectory(atPath: root.path)
            .map { root.appendingPathComponent($0) }
    }

    private func defaultRemoveCacheDelta(under root: URL, baselinePaths: Set<String>) throws {
        let createdEntries = try defaultCacheEntries(under: root)
            .filter { !baselinePaths.contains($0.standardizedFileURL.path) }
            .sorted { $0.pathComponents.count > $1.pathComponents.count }
        for entry in createdEntries {
            do {
                try FileManager.default.removeItem(at: entry)
            } catch CocoaError.fileNoSuchFile {
                // The store keeps flushing its index after the player stops:
                // a temp file renamed away since the listing is already gone.
                continue
            }
        }
    }

    private func defaultIsInside(_ candidate: URL, root: URL) -> Bool {
        let candidatePath = candidate.standardizedFileURL.path
        let rootPath = root.standardizedFileURL.path
        return candidatePath == rootPath || candidatePath.hasPrefix(rootPath + "/")
    }

    private func defaultWaitForCacheFact(
        _ description: String,
        condition: () throws -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(45))
        while try !condition() {
            guard clock.now < deadline else {
                throw DefaultCacheFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private struct DefaultCacheFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
