import Testing
@testable import Kithara

@Suite("KitharaPlayerItem")
struct KitharaPlayerItemTests {

    @Test("init sets id and url")
    func initSetsIdAndUrl() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
        #expect(!item.id.isEmpty)
        #expect(item.url == "https://example.com/song.mp3")
    }

    @Test("preferred bitrate defaults to zero")
    func preferredBitrateDefaults() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
        #expect(item.preferredPeakBitrate == 0)
        #expect(item.preferredPeakBitrateForExpensiveNetworks == 0)
    }

    @Test("set preferred bitrate roundtrip")
    func setPreferredBitrate() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
        item.preferredPeakBitrate = 128_000
        #expect(item.preferredPeakBitrate == 128_000)
    }

    @Test("each item gets unique id")
    func uniqueIds() {
        let a = KitharaPlayerItem(url: "https://example.com/a.mp3")
        let b = KitharaPlayerItem(url: "https://example.com/b.mp3")
        #expect(a.id != b.id)
    }
}
