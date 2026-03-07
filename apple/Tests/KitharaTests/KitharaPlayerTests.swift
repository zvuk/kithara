import Testing
@testable import Kithara

@Suite("KitharaPlayer")
struct KitharaPlayerTests {

    @Test("init creates player with unknown status")
    func initCreatesPlayerWithUnknownStatus() {
        let player = KitharaPlayer()
        #expect(player.status == .unknown)
        #expect(player.currentTime == nil)
        #expect(player.duration == nil)
    }

    @Test("default rate is 1.0")
    func defaultRateIsOne() {
        let player = KitharaPlayer()
        #expect(player.defaultRate == 1.0)
    }

    @Test("items starts empty")
    func itemsStartsEmpty() {
        let player = KitharaPlayer()
        #expect(player.items.isEmpty)
    }

    @Test("removeAllItems on empty queue does not crash")
    func removeAllItemsOnEmpty() {
        let player = KitharaPlayer()
        player.removeAllItems()
        #expect(player.items.isEmpty)
    }

    @Test("snapshot returns consistent state")
    func snapshotReturnsConsistentState() {
        let player = KitharaPlayer()
        let snap = player.snapshot
        #expect(snap.rate == 0.0)
        #expect(snap.defaultRate == 1.0)
        #expect(snap.currentTime == nil)
        #expect(snap.duration == nil)
    }
}
