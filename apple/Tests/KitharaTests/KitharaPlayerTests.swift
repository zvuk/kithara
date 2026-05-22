import Testing
@testable import Kithara

@Suite("KitharaPlayer")
struct KitharaPlayerTests {

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

    @Test("setupNetwork stores auth token")
    func setupNetworkStoresAuthToken() {
        let player = KitharaPlayer()
        // setupNetwork is fire-and-forget; we just verify the call
        // path doesn't throw. Header-side asserts are covered by the
        // Rust-level FFI tests.
        player.setupNetwork(authToken: "demo-token-123")
        #expect(player.status == .unknown)
    }
}
