import Foundation
import Testing
@testable import Kithara

@Suite("KitharaPlayerItem")
struct KitharaPlayerItemTests {

    @Test("init sets default audioId and url")
    func initSetsIdAndUrl() {
        let urlString = "https://example.com/song.mp3"
        let item = KitharaPlayerItem(url: urlString)
        #expect(item.audioId >= 0)
        #expect(item.id == item.uuid)
        #expect(item.url == URL(string: urlString))
    }

    @Test("init surfaces caller audioId")
    func initSurfacesCallerAudioId() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3", audioId: 42)
        #expect(item.audioId == 42)
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

    @Test("default audioId stays monotonic")
    func defaultAudioIdsAreMonotonic() {
        let a = KitharaPlayerItem(url: "https://example.com/a.mp3")
        let b = KitharaPlayerItem(url: "https://example.com/b.mp3")
        #expect(a.audioId < b.audioId)
    }

    @Test("two items with the same caller audioId get distinct uuid")
    func distinctIdentifiersForSameUrl() {
        let a = KitharaPlayerItem(url: "https://example.com/track.mp3", audioId: 7)
        let b = KitharaPlayerItem(url: "https://example.com/track.mp3", audioId: 7)
        #expect(a.audioId == b.audioId)
        #expect(a.uuid != b.uuid)
        #expect(a.id != b.id)
    }
}
