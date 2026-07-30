import AVFAudio
import Foundation
import Testing

@Suite("Hosted iOS Unit Tests")
struct HostedTestScaffoldingTests {
    @Test("runs inside KitharaDemo with an audio session")
    func hostAndAudioSessionAreAvailable() {
        #expect(Bundle.main.bundleURL.pathExtension == "app")
        #expect(Bundle.main.bundleIdentifier == "com.kithara.demo")

        let audioSession = AVAudioSession.sharedInstance()
        #expect(!audioSession.category.rawValue.isEmpty)
    }
}
