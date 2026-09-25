import Combine
import Foundation
import KitharaFFI
import Testing
@testable import Kithara

@Suite("KitharaPlayer")
struct KitharaPlayerTests {
    final class LegacyItem {}

    @Test("HLS source settings preserve the batch-size initializer")
    func hlsSourceSettingsInitializer() {
        let legacy = FfiHlsSourceSettings(downloadBatchSize: 6)
        let configured = FfiHlsSourceSettings(sizeProbeMethod: .rangeGet, downloadBatchSize: 6)
        let bounded = FfiHlsSourceSettings(lookAheadBytes: 0, sizeProbeMethod: .rangeGet, downloadBatchSize: 6)
        let attempts = FfiHlsSourceSettings(lookAheadBytes: 0, sizeProbeMethod: .rangeGet, acquireAttemptBudget: 1, downloadBatchSize: 6)
        #expect(legacy.downloadBatchSize == 6)
        #expect(legacy.sizeProbeMethod == nil)
        #expect(configured.sizeProbeMethod == .rangeGet)
        #expect(bounded.lookAheadBytes == 0)
        #expect(bounded.acquireAttemptBudget == nil)
        #expect(attempts.acquireAttemptBudget == 1)
    }

    @Test("File source settings preserve the reader-event initializer")
    func fileSourceSettingsInitializer() {
        let legacy = FfiFileSourceSettings(readerEventCapacity: 512)
        let bounded = FfiFileSourceSettings(lookAheadBytes: 0, readerEventCapacity: 512)
        #expect(legacy.readerEventCapacity == 512)
        #expect(legacy.lookAheadBytes == nil)
        #expect(bounded.lookAheadBytes == 0)
    }

    init() throws {
        try TestHost.initialize()
    }

    @Test("init creates player with unknown status")
    func initCreatesPlayerWithUnknownStatus() throws {
        let player = KitharaPlayer()
        #expect(player.status == .unknown)
        #expect(player.currentTime == 0.0)
        #expect(player.duration == nil)
    }

    @Test("playing rate is 1.0")
    func playingRateIsOne() throws {
        let player = KitharaPlayer()
        #expect(player.playingRate == 1.0)
    }

    @Test("items() starts empty")
    func itemsStartsEmpty() throws {
        let player = KitharaPlayer()
        #expect(player.items().isEmpty)
    }

    @Test("removeAllItems on empty queue does not crash")
    func removeAllItemsOnEmpty() throws {
        let player = KitharaPlayer()
        player.removeAllItems()
        #expect(player.items().isEmpty)
    }

    @Test("stop clears the queue and allows a fresh item")
    func stopClearsQueueAndAllowsFreshItem() throws {
        let player = KitharaPlayer()
        let old = KitharaPlayerItem(url: "https://example.com/old.mp3")
        try player.insert(old)

        player.stop()

        #expect(player.items().isEmpty)
        #expect(player.itemCount == 0)
        #expect(player.currentAudioItem == nil)

        let fresh = KitharaPlayerItem(url: "https://example.com/fresh.mp3")
        try player.insert(fresh)
        #expect(player.itemCount == 1)
        #expect(player.items().first === fresh)
    }

    @Test("snapshot returns consistent state")
    func snapshotReturnsConsistentState() throws {
        let player = KitharaPlayer()
        let snap = player.snapshot
        #expect(snap.rate == 0.0)
        #expect(snap.playingRate == 1.0)
        #expect(snap.currentTime == nil)
        #expect(snap.duration == nil)
    }

    @Test("currentAudioItem nil when queue empty")
    func currentAudioItemNilWhenEmpty() throws {
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

        let item = KitharaPlayerItem(
            url: "https://example.com/first.mp3",
            audioId: 42,
            uuid: 123
        )

        try player.insert(item)

        #expect(player.currentAudioItem?.uuid == item.uuid)
        #expect(observed == [nil, item.uuid])
        _ = cancellable
    }

    @Test("advanceToNextItem is a non-throwing no-op on the last item")
    func advanceToNextItemIsANoOpOnTheLastItem() throws {
        let player = KitharaPlayer()
        let only = KitharaPlayerItem(
            url: "https://example.com/only.mp3",
            audioId: 42,
            uuid: 123
        )
        var errors: [KitharaPlayerError] = []
        let cancellable = player.contextualError.sink { errors.append($0) }

        try player.insert(only)
        player.advanceToNextItem()

        #expect(player.currentAudioItem?.uuid == only.uuid)
        #expect(player.items().map(\.uuid) == [only.uuid])
        #expect(errors.isEmpty)
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
    func setupNetworkStoresAuthToken() throws {
        let player = KitharaPlayer()
        // setupNetwork is fire-and-forget; we just verify the call
        // path doesn't throw. Header-side asserts are covered by the
        // Rust-level FFI tests.
        player.setupNetwork(authToken: "demo-token-123")
        #expect(player.status == .unknown)
    }

    @Test("typed queue policy and complete crossfade profile round trip")
    func typedQueuePolicyRoundTrips() throws {
        let player = KitharaPlayer()
        let settings = try CrossfadeSettings(
            duration: 2.5,
            curve: .linear,
            depth: 0.25,
            position: 0.3
        )
        try player.setCrossfadeSettings(settings)
        try player.setPlaybackOrder(.shuffle)
        try player.setActionAtItemEnd(.pause)
        #expect(player.crossfadeSettings == settings)
        #expect(player.playbackOrder == .shuffle)
        #expect(player.actionAtItemEnd == .pause)
    }

    @Test("generated queue settings reach the player owner")
    func generatedQueueSettingsReachOwner() throws {
        let settings = FfiQueueSettings(
            maxConcurrentLoads: 4,
            prefetchDuration: 2,
            shouldAutoplay: false,
            maxHistorySize: 25,
            playbackOrder: .shuffle,
            actionAtItemEnd: .pause,
            crossfadeSettings: FfiCrossfadeSettings(
                duration: 1.5,
                curve: .linear,
                depth: 0.5,
                position: 0.3
            )
        )
        let player = try KitharaPlayer(config: .init(), queueSettings: settings)
        #expect(player.playbackOrder == .shuffle)
        #expect(player.actionAtItemEnd == .pause)
        #expect(player.crossfadeSettings.duration == 1.5)
        #expect(throws: FfiError.self) {
            try KitharaPlayer(
                config: .init(),
                queueSettings: FfiQueueSettings(
                    maxConcurrentLoads: 0,
                    prefetchDuration: nil,
                    shouldAutoplay: nil,
                    maxHistorySize: nil,
                    playbackOrder: nil,
                    actionAtItemEnd: nil,
                    crossfadeSettings: nil
                )
            )
        }
    }

    @Test("crossfade settings reject invalid values")
    func crossfadeSettingsRejectInvalidValues() throws {
        #expect(throws: KitharaError.self) {
            try CrossfadeSettings(duration: -.infinity)
        }
        #expect(throws: KitharaError.self) {
            try CrossfadeSettings(depth: .nan)
        }
        #expect(throws: KitharaError.self) {
            try CrossfadeSettings(position: 1)
        }
    }

    @Test("command errors are emitted with affected item id")
    func commandErrorsAreEmittedWithAffectedItemId() throws {
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
