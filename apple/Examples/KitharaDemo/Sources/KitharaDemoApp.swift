import Kithara
import SwiftUI

@main
struct KitharaDemoApp: App {
    init() {
        Kithara.initLogging(level: .debug)
    }

    #if os(macOS)
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #endif

    var body: some Scene {
        WindowGroup {
            PlayerView()
                #if os(macOS)
                .onAppear {
                    // CLI-launched executables (not .app bundles) don't
                    // automatically become the active app on macOS,
                    // so keyboard events (including Cmd+V) are not delivered.
                    NSApplication.shared.setActivationPolicy(.regular)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
                #endif
        }
        #if os(macOS)
        .commands {
            TextEditingCommands()
        }
        // Quit the process when the last window is closed.
        .defaultSize(width: 520, height: 760)
        #endif
    }
}

#if os(macOS)
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool {
        true
    }
}
#endif
