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

    @Test("init surfaces caller uuid")
    func initSurfacesCallerUuid() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3", uuid: 123_456)
        #expect(item.uuid == 123_456)
        #expect(item.id == 123_456)
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

    @Test("structured playback progress accepts HLS seek landing tolerance")
    func structuredProgressAcceptsSeekLandingTolerance() {
        let item = KitharaPlayerItem(url: "https://example.com/song.m3u8")
        let ranges = [ItemLoadedRange(start: 211.6, duration: 10)]
        let startTolerance: TimeInterval = 5

        #expect(item.isPlayable(
            progress: PlaybackProgress(value: 0.74, time: 208.3, duration: 283.16),
            ranges: ranges,
            startTolerance: startTolerance
        ))
        #expect(!item.isPlayable(
            progress: PlaybackProgress(value: 0.70, time: 205.0, duration: 283.16),
            ranges: ranges,
            startTolerance: startTolerance
        ))
    }

    @Test("structured playback progress keeps end progress playable with buffered ranges")
    func structuredProgressKeepsEndProgressPlayableWithRanges() {
        let item = KitharaPlayerItem(url: "https://example.com/song.mp3")
        let ranges = [ItemLoadedRange(start: 0, duration: 30)]

        #expect(item.isPlayable(
            progress: PlaybackProgress(value: 1, time: 45, duration: 45),
            ranges: ranges
        ))
        #expect(!item.isPlayable(
            progress: PlaybackProgress(value: 1, time: 45, duration: 45),
            ranges: []
        ))
    }

    @Test("playability accepts client-shaped progress and ranges")
    func playabilityAcceptsClientShapedProgressAndRanges() {
        let item = KitharaPlayerItem(url: "https://example.com/song.m3u8")
        let progress = ExternalProgress(value: 0.74, time: 208.3, duration: 283.16)
        let ranges = [ExternalRange(start: 211.6, duration: 10)]

        #expect(item.isPlayable(
            progress: progress,
            ranges: ranges,
            startTolerance: 5
        ))
    }
}

private struct ExternalProgress: PlaybackProgressSource {
    let value: TimeInterval
    let time: TimeInterval
    let duration: TimeInterval
}

private struct ExternalRange: ItemLoadedRangeSource {
    let start: TimeInterval
    let duration: TimeInterval
}
