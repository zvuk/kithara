import Foundation
import Testing
@testable import Kithara

@Suite("KitharaPlayer first use")
struct KitharaPlayerFirstUseTests {
    @Test("concurrent default players share one initialized host")
    func concurrentDefaultPlayers() {
        let start = DispatchSemaphore(value: 0)
        let finished = DispatchGroup()
        let players = LockedValue<[KitharaPlayer]>([])

        for _ in 0..<2 {
            finished.enter()
            DispatchQueue.global(qos: .userInitiated).async {
                start.wait()
                let player = KitharaPlayer()
                players.withLock { $0.append(player) }
                finished.leave()
            }
        }

        start.signal()
        start.signal()
        finished.wait()
        #expect(players.withLock { $0.count } == 2)

        do {
            try KitharaHost.initialize()
            Issue.record("the default host should already be initialized")
        } catch KitharaHost.InitializationError.alreadyInitialized {
            // Both players joined the same process host.
        } catch {
            Issue.record("unexpected host state: \(error)")
        }
    }

    @Test("a native 65-band player keeps the existing constructor")
    func nativeEqLayoutAboveWebLimit() {
        let player = KitharaPlayer(config: .init(eqBandCount: 65))
        #expect(player.eqBandCount == 65)
        player.setEqGain(band: 64, gainDb: 3)
        #expect(player.eqGain(band: 64) == 3)
        player.resetEq()
        #expect(player.eqGain(band: 64) == 0)
    }
}
