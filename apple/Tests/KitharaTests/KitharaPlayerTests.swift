import Testing
@testable import Kithara

@Suite("KitharaPlayer")
struct KitharaPlayerTests {

    @Test("init creates player with unknown status")
    func initCreatesPlayerWithUnknownStatus() {
        let player = KitharaPlayer()
        #expect(player.status == .unknown)
        #expect(player.currentTime == 0)
        #expect(player.duration == nil)
        #expect(player.error == nil)
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
}
