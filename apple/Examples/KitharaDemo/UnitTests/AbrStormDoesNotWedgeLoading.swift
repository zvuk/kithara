import Combine
import Foundation
import Kithara
import Testing

extension IntegrationRegressionsIOS {
    @Test("An ABR mode storm does not wedge loading")
    func abrStormDoesNotWedgeLoading() async throws {
        let cacheURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("abr-storm-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: cacheURL,
            withIntermediateDirectories: true
        )

        let player = KitharaPlayer(config: .init(store: AssetStore(root: cacheURL.path)))
        let item = KitharaPlayerItem(
            url: try await throttledAbrMasterURL().absoluteString
        )
        let facts = AbrFacts()
        let variantsCancellable = item.variantsDiscovered.sink { variants in
            facts.record(variants)
        }
        let readyCancellable = item.readyToPlay.sink {
            facts.recordReady()
        }
        defer {
            player.stop()
            try? FileManager.default.removeItem(at: cacheURL)
            _ = variantsCancellable
            _ = readyCancellable
        }

        try player.insert(item)
        player.play()
        try await waitForAbrFact("the four HLS variants to be discovered") {
            facts.variants.count == 4
        }

        let variants = facts.variants
        try #require(
            !facts.isReady && player.currentTime <= 0.1,
            """
            precondition: the throttled media playlist finished \
            loading before the ABR storm; ready=\(facts.isReady), \
            current=\(player.currentTime)s
            """
        )

        for round in 0..<16 {
            if round.isMultiple(of: 2) {
                let variant = variants[(round / 2) % variants.count]
                player.setAbrMode(.manual(variantIndex: variant.index))
            } else {
                player.setAbrMode(.auto)
            }
        }
        player.setAbrMode(.auto)

        let advanced = await reachedAbrFact(deadline: .seconds(60)) {
            player.currentTime > 0.5
        }
        #expect(
            advanced,
            """
            playback never advanced after the ABR auto/manual storm \
            landed during HLS loading; current=\(player.currentTime)s, \
            rate=\(player.currentRate), ready=\(facts.isReady)
            """
        )
        guard advanced else {
            return
        }
        try await waitForAbrFact("the long HLS duration to settle after playback advanced") {
            (player.duration ?? 0) >= 180
        }
    }

    private func throttledAbrMasterURL() async throws -> URL {
        let specs = [
            AbrVariantSpec(
                bandwidth: 66_005,
                codecs: "mp4a.40.2",
                playlist: "index-slq-a1.m3u8",
                initialization: "init-slq-a1.mp4"
            ),
            AbrVariantSpec(
                bandwidth: 134_107,
                codecs: "mp4a.40.2",
                playlist: "index-smq-a1.m3u8",
                initialization: "init-smq-a1.mp4"
            ),
            AbrVariantSpec(
                bandwidth: 269_930,
                codecs: "mp4a.40.2",
                playlist: "index-shq-a1.m3u8",
                initialization: "init-shq-a1.mp4"
            ),
            AbrVariantSpec(
                bandwidth: 988_758,
                codecs: "fLaC",
                playlist: "index-slossless-a1.m3u8",
                initialization: "init-slossless-a1.mp4"
            ),
        ]

        var playlistURLs: [URL] = []
        for spec in specs {
            let sourceURL = try TestServerFixture.asset("hls/\(spec.playlist)")
            let playlist = try await abrPlaylistText(at: sourceURL)
            let rewritten = try rewriteAbrPlaylist(playlist, spec: spec)
            let handle = try await TestServerFixture.registerBehavior(
                .init(
                    content: .bytes(
                        Data(rewritten.utf8),
                        contentType: "application/vnd.apple.mpegurl"
                    ),
                    delivery: .throttle(chunk: 16, delayMilliseconds: 50)
                )
            )
            playlistURLs.append(handle.childURL(spec.playlist))
        }

        var master = "#EXTM3U\n"
        for (spec, playlistURL) in zip(specs, playlistURLs) {
            master += """
            #EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=\(spec.bandwidth),\
            CODECS="\(spec.codecs)",AVERAGE-BANDWIDTH=\(spec.bandwidth)
            \(playlistURL.absoluteString)

            """
        }
        let masterHandle = try await TestServerFixture.registerBehavior(
            .init(
                content: .bytes(
                    Data(master.utf8),
                    contentType: "application/vnd.apple.mpegurl"
                ),
                delivery: .normal
            )
        )
        return masterHandle.childURL("master.m3u8")
    }

    private func abrPlaylistText(at url: URL) async throws -> String {
        let (data, response) = try await URLSession.shared.data(from: url)
        let http = try #require(
            response as? HTTPURLResponse,
            "precondition: \(url.lastPathComponent) returned a non-HTTP response"
        )
        try #require(
            http.statusCode == 200,
            """
            precondition: \(url.lastPathComponent) returned HTTP \
            \(http.statusCode)
            """
        )
        return try #require(
            String(data: data, encoding: .utf8),
            "precondition: \(url.lastPathComponent) was not UTF-8"
        )
    }

    private func rewriteAbrPlaylist(
        _ playlist: String,
        spec: AbrVariantSpec
    ) throws -> String {
        let initializationURL = try TestServerFixture.asset(
            "hls/\(spec.initialization)"
        )
        var rewritten: [String] = []

        for substring in playlist.split(
            separator: "\n",
            omittingEmptySubsequences: false
        ) {
            let line = String(substring)
            if line.hasPrefix("#EXT-X-MAP:") {
                rewritten.append(
                    line.replacingOccurrences(
                        of: spec.initialization,
                        with: initializationURL.absoluteString
                    )
                )
            } else if !line.isEmpty && !line.hasPrefix("#") {
                rewritten.append(
                    try TestServerFixture.asset("hls/\(line)").absoluteString
                )
            } else {
                rewritten.append(line)
            }
        }
        return rewritten.joined(separator: "\n")
    }

    private func reachedAbrFact(
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

    private func waitForAbrFact(
        _ description: String,
        condition: () -> Bool
    ) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(30))
        while !condition() {
            guard clock.now < deadline else {
                throw AbrFactTimeout(description)
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

private struct AbrVariantSpec {
    let bandwidth: Int
    let codecs: String
    let playlist: String
    let initialization: String
}

private final class AbrFacts: @unchecked Sendable {
    private let lock = NSLock()
    private var discovered: [Variant] = []
    private var ready = false

    var variants: [Variant] {
        lock.lock()
        defer { lock.unlock() }
        return discovered
    }

    var isReady: Bool {
        lock.lock()
        defer { lock.unlock() }
        return ready
    }

    func record(_ variants: [Variant]) {
        lock.lock()
        defer { lock.unlock() }
        discovered = variants
    }

    func recordReady() {
        lock.lock()
        defer { lock.unlock() }
        ready = true
    }
}

private struct AbrFactTimeout: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = "Timed out waiting for \(description)"
    }
}
