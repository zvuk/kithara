import Combine
import Foundation
import KitharaFFI
import Testing
@testable import Kithara

@Suite("KitharaPlayer")
struct KitharaPlayerTests {
    final class LegacyItem {}

    @Test("init creates player with unknown status")
    func initCreatesPlayerWithUnknownStatus() {
        let player = KitharaPlayer()
        #expect(player.status == .unknown)
        #expect(player.currentTime == 0.0)
        #expect(player.duration == nil)
    }

    @Test("playing rate is 1.0")
    func playingRateIsOne() {
        let player = KitharaPlayer()
        #expect(player.playingRate == 1.0)
    }

    @Test("items() starts empty")
    func itemsStartsEmpty() {
        let player = KitharaPlayer()
        #expect(player.items().isEmpty)
    }

    @Test("removeAllItems on empty queue does not crash")
    func removeAllItemsOnEmpty() {
        let player = KitharaPlayer()
        player.removeAllItems()
        #expect(player.items().isEmpty)
    }

    @Test("snapshot returns consistent state")
    func snapshotReturnsConsistentState() {
        let player = KitharaPlayer()
        let snap = player.snapshot
        #expect(snap.rate == 0.0)
        #expect(snap.playingRate == 1.0)
        #expect(snap.currentTime == nil)
        #expect(snap.duration == nil)
    }

    @Test("currentAudioItem nil when queue empty")
    func currentAudioItemNilWhenEmpty() {
        let player = KitharaPlayer()
        #expect(player.currentAudioItem == nil)
    }

    @Test("first inserted item becomes current before playback")
    func firstInsertedItemBecomesCurrentBeforePlayback() throws {
        let player = KitharaPlayer()
        var observed: [Int64?] = []
        let cancellable = player.currentItem.sink { item in
            observed.append(item?.uuid)
        }

        let assetURL = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("assets/test.mp3")
        let item = KitharaPlayerItem(
            url: assetURL.absoluteString,
            audioId: 42,
            uuid: 123
        )

        try player.insert(item)

        #expect(player.currentAudioItem?.uuid == item.uuid)
        #expect(observed == [nil, item.uuid])
        _ = cancellable
    }

    @Test("represented item follows queue identity")
    func representedItemFollowsQueueIdentity() throws {
        let player = KitharaPlayer()
        let represented = LegacyItem()
        let item = KitharaPlayerItem(
            url: "https://example.com/represented.mp3",
            audioId: 42,
            uuid: 123
        )

        try player.insert(item, representing: represented)

        #expect(player.currentItemRepresentation(as: LegacyItem.self) === represented)
        #expect(player.itemRepresentations(as: LegacyItem.self).first === represented)

        try player.remove(item)

        #expect(player.currentItemRepresentation(as: LegacyItem.self) == nil)
        #expect(player.itemRepresentations(as: LegacyItem.self).isEmpty)
    }

    @Test("setupNetwork stores auth token")
    func setupNetworkStoresAuthToken() {
        let player = KitharaPlayer()
        // setupNetwork is fire-and-forget; we just verify the call
        // path doesn't throw. Header-side asserts are covered by the
        // Rust-level FFI tests.
        player.setupNetwork(authToken: "demo-token-123")
        #expect(player.status == .unknown)
    }

    @Test("command errors are emitted with affected item id")
    func commandErrorsAreEmittedWithAffectedItemId() {
        let player = KitharaPlayer()
        var observed: [KitharaPlayerError] = []
        let cancellable = player.contextualError.sink { error in
            observed.append(error)
        }

        let item = KitharaPlayerItem(
            url: "https://example.com/not-in-queue.mp3",
            audioId: 42,
            uuid: 123
        )
        var thrown: Error?
        do {
            try player.remove(item)
        } catch {
            thrown = error
        }

        #expect(thrown is KitharaError)
        #expect(observed.count == 1)
        #expect(observed.first?.itemId == item.audioId)
        if case .command = observed.first {
        } else {
            Issue.record("expected command error")
        }
        _ = cancellable
    }

    @Test("track load failures preserve reason and item id")
    func trackLoadFailuresPreserveReasonAndItemId() throws {
        let player = KitharaPlayer()
        let item = KitharaPlayerItem(
            url: "https://example.com/failing.mp3",
            audioId: 42,
            uuid: 123
        )
        try player.insert(item)

        let error = player.playerError(
            from: .trackLoadFailed(
                itemId: item.ffiTrackId,
                reason: "cache rejected",
                autoSkipped: true
            )
        )

        guard case let .playback(.itemFailed(reason), itemId) = error else {
            Issue.record("expected attributed item load failure")
            return
        }
        #expect(reason == "cache rejected")
        #expect(itemId == item.audioId)
    }
}
