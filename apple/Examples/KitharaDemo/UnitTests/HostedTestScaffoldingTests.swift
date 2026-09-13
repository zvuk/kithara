import AVFAudio
import Foundation
import Testing

@Suite("Hosted iOS Unit Tests")
struct HostedTestScaffoldingTests {
    @Test("configures audio before hosted tests create players")
    func hostConfiguresAudioSessionBeforeTests() {
        #expect(Bundle.main.bundleURL.pathExtension == "app")
        #expect(Bundle.main.bundleIdentifier == "com.kithara.demo")

        let audioSession = AVAudioSession.sharedInstance()
        #expect(audioSession.category == .playback)
        #expect(audioSession.preferredSampleRate == 48_000)
        #expect(audioSession.preferredIOBufferDuration == 128.0 / 48_000)
        #expect(audioSession.sampleRate > 0)
        #expect(audioSession.ioBufferDuration > 0)
        #expect(audioSession.sampleRate * audioSession.ioBufferDuration > 0)
    }
}
