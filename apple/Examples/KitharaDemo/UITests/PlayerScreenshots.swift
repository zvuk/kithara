import XCTest

/// Drives the demo through each tab so a CI/agent run can compare the
/// iOS UI against the Android/Rust counterparts. Each test attaches a
/// full-screen screenshot to its result, tagged with `keepAlways` so
/// the .xcresult bundle preserves them on success too.
final class PlayerScreenshots: XCTestCase {
    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    func testCapturePlaylistTab() {
        let app = XCUIApplication()
        app.launch()
        // The Playlist tab is selected by default — wait for the
        // header to confirm the UI rendered, then capture.
        XCTAssertTrue(app.staticTexts["Kithara"].waitForExistence(timeout: 5))
        attachScreenshot(named: "ios-playlist")
    }

    func testCaptureEqTab() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.staticTexts["Kithara"].waitForExistence(timeout: 5))
        // Tab buttons show their rawValue ("EQ", "Settings") as label.
        let eqTab = app.buttons["EQ"]
        XCTAssertTrue(eqTab.waitForExistence(timeout: 5))
        eqTab.tap()
        attachScreenshot(named: "ios-eq")
    }

    func testCaptureSettingsTab() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.staticTexts["Kithara"].waitForExistence(timeout: 5))
        let settingsTab = app.buttons["Settings"]
        XCTAssertTrue(settingsTab.waitForExistence(timeout: 5))
        settingsTab.tap()
        attachScreenshot(named: "ios-settings")
    }

    private func attachScreenshot(named name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
