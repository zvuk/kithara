import Foundation
import Testing
@testable import Kithara

@Suite("KitharaPlayerItem")
struct KitharaPlayerItemTests {

    @Test("init sets monotonic audioId and url")
    func initSetsIdAndUrl() {
        let urlString = "https://example.com/song.mp3"
        let item = KitharaPlayerItem(url: urlString)
        #expect(item.audioId == item.id)
        #expect(item.url == URL(string: urlString))
    }

    @Test("preferred bitrate frozen at construction defaults to zero")
    func preferredBitrateDefaults() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
        #expect(item.preferredPeakBitrate == 0)
        #expect(item.preferredPeakBitrateForExpensiveNetworks == 0)
    }

    @Test("preferred bitrate from constructor is surfaced")
    func preferredBitrateFromInit() {
        let item = KitharaPlayerItem(
            url: "https://example.com/song.mp3",
            preferredPeakBitrate: 128_000,
            preferredPeakBitrateForExpensiveNetworks: 96_000
        )
        #expect(item.preferredPeakBitrate == 128_000)
        #expect(item.preferredPeakBitrateForExpensiveNetworks == 96_000)
    }

    @Test("each item gets monotonic audioId")
    func uniqueAudioIds() {
        let a = KitharaPlayerItem(url: "https://example.com/a.mp3")
        let b = KitharaPlayerItem(url: "https://example.com/b.mp3")
        #expect(a.audioId < b.audioId)
    }

    @Test("two items with the same URL get distinct audioId and uuid")
    func distinctIdentifiersForSameUrl() {
        let a = KitharaPlayerItem(url: "https://example.com/track.mp3")
        let b = KitharaPlayerItem(url: "https://example.com/track.mp3")
        #expect(a.audioId != b.audioId)
        #expect(a.uuid != b.uuid)
    }
}
