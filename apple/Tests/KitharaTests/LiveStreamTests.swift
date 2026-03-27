import Foundation
import Testing
@testable import Kithara

@Suite("Live Stream")
struct LiveStreamTests {

    @Test("zvuk MP3 loads and reports duration")
    func zvukLoadsWithDuration() async throws {
        let player = KitharaPlayer()
        let item = KitharaPlayerItem(url: "https://cdn-edge.zvq.me/track/streamhq?id=151585912")
        item.load()

        var receivedDuration: Double?
        let expectation = item.eventPublisher
            .compactMap { event -> Double? in
                if case let .durationChanged(seconds) = event { return seconds }
                if case let .error(msg) = event { Issue.record("item error: \(msg)"); return nil }
                return nil
            }
            .first()
            .values

        for await duration in expectation {
            receivedDuration = duration
            break
        }

        #expect(receivedDuration != nil, "duration must be reported")
        if let dur = receivedDuration {
            #expect(dur > 30.0, "expected >30s, got \(dur)s")
        }
    }

    @Test("silvercomet MP3 loads and reports duration")
    func silvercometLoadsWithDuration() async throws {
        let player = KitharaPlayer()
        let item = KitharaPlayerItem(url: "https://stream.silvercomet.top/track.mp3")
        item.load()

        var receivedDuration: Double?
        let expectation = item.eventPublisher
            .compactMap { event -> Double? in
                if case let .durationChanged(seconds) = event { return seconds }
                if case let .error(msg) = event { Issue.record("item error: \(msg)"); return nil }
                return nil
            }
            .first()
            .values

        for await duration in expectation {
            receivedDuration = duration
            break
        }

        #expect(receivedDuration != nil, "duration must be reported")
        if let dur = receivedDuration {
            #expect(dur > 30.0, "expected >30s, got \(dur)s")
        }
    }
}
