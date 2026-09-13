import AVFAudio
import Kithara
import SwiftUI

@main
struct KitharaDemoApp: App {
    #if os(iOS)
    private let isAudioSessionReady: Bool
    #endif

    init() {
        Kithara.initLogging(level: .debug)
        #if os(iOS)
        isAudioSessionReady = Self.configureAudioSession()
        #endif
    }

    #if os(macOS)
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #endif

    /// Hosted unit tests need this process and its audio session, not its UI.
    /// Building `PlayerView` would start a second player against the demo's own
    /// stream, which competes with the tests for the shared asset cache.
    private static var isHostingTests: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
            || NSClassFromString("XCTestCase") != nil
    }

    var body: some Scene {
        WindowGroup {
            if Self.isHostingTests {
                Color.clear
            } else {
                #if os(iOS)
                if !isAudioSessionReady {
                    Text("KitharaDemo could not configure audio playback.")
                } else {
                    PlayerView()
                }
                #else
                PlayerView()
                .onAppear {
                    // CLI-launched executables (not .app bundles) don't
                    // automatically become the active app on macOS,
                    // so keyboard events (including Cmd+V) are not delivered.
                    NSApplication.shared.setActivationPolicy(.regular)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
                #endif
            }
        }
        #if os(macOS)
        .commands {
            TextEditingCommands()
        }
        // Quit the process when the last window is closed.
        .defaultSize(width: 520, height: 760)
        #endif
    }

    #if os(iOS)
    /// App-owned policy, applied before `PlayerView` can allocate its player.
    private static func configureAudioSession() -> Bool {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playback)
            try session.setPreferredSampleRate(48_000)
            try session.setPreferredIOBufferDuration(128.0 / 48_000)
            try session.setActive(true)

            let sampleRate = session.sampleRate
            let bufferDuration = session.ioBufferDuration
            let frames = sampleRate * bufferDuration
            print(
                "[KitharaDemo] audio session: sampleRate=\(sampleRate), "
                    + "ioBufferDuration=\(bufferDuration), frames=\(frames)"
            )
            guard sampleRate > 0, bufferDuration > 0 else {
                print("[KitharaDemo] audio session granted invalid values")
                return false
            }
            return true
        } catch {
            print("[KitharaDemo] audio session setup failed: \(error)")
            return false
        }
    }
    #endif
}

#if os(macOS)
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool {
        true
    }
}
#endif
