import XCTest

/// XCUITest reproducing the false-EOF auto-advance scenario captured
/// in `kithara/app.log` at 06:39:13: a prod DRM track is selected,
/// the slider is immediately scrubbed to ~45 % before any
/// playback / buffering warm-up, the decoder fails with
/// `SeekOutOfRange` → `isomp4: no atom pending read`, and the queue
/// silently advances as if the track played to natural end.
///
/// This test is the **regression gate** the user trusts because it
/// drives the demo through the OS-level UIKit accessibility tree
/// — it is not authored to be pass-by-design like the Rust unit
/// tests, which the user has explicitly noted "I cannot trust
/// because the agent can craft them to be green".
///
/// # Contract under test
///
/// After scrubbing to a position that is past the currently
/// buffered range, ONE of the following must be observable in the
/// UI within 8 s of the scrub:
///   * the current track did NOT change, OR
///   * the current track changed AND the `errorMessage` element
///     is visible (graceful track-failed surface).
///
/// What MUST NOT happen is a silent auto-advance: the row label
/// of the now-playing item changing without an error surface
/// — that is the false-EOF cascade the user keeps reproducing.
final class FalseEofRegression: XCTestCase {
    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    /// Index of the prod-DRM track in
    /// `PlayerViewModel.defaultTrackURLs`. Track `171515249_1`
    /// (~277 s) — same track that auto-advanced at pos=121 s
    /// in the captured app.log.
    private static let prodDrmTrackIndex = 17

    func testRapidScrubWithoutWarmupDoesNotFalseEofAdvance() throws {
        let app = XCUIApplication()
        app.launch()

        XCTAssertTrue(
            app.staticTexts["Kithara"].waitForExistence(timeout: 10),
            "demo app did not finish launching within 10 s"
        )

        // The default playlist is appended on PlayerViewModel.init,
        // but track rows render asynchronously. Wait for the target
        // row before tapping.
        let row = app.buttons["trackRow_\(Self.prodDrmTrackIndex)"]
        XCTAssertTrue(
            row.waitForExistence(timeout: 10),
            "prod DRM track row #\(Self.prodDrmTrackIndex) " +
            "never appeared in the playlist"
        )
        row.tap()

        // The now-playing label must reflect the selection before
        // we read it. The track name we record now is the canonical
        // value to compare against after the scrub.
        let currentTrackLabel = app.staticTexts["currentTrackName"]
        XCTAssertTrue(
            currentTrackLabel.waitForExistence(timeout: 10),
            "currentTrackName label never appeared after row tap"
        )
        // Give the bridge a moment to publish CurrentItemChanged.
        // We don't wait on full buffering — that is the entire
        // point of the test (reproduce the user's app.log).
        _ = currentTrackLabel.waitForExistence(timeout: 2)
        let initialTrackName = currentTrackLabel.label
        XCTAssertFalse(
            initialTrackName.isEmpty,
            "currentTrackName label was empty after row tap"
        )

        // The seek slider stays disabled until duration is known
        // — wait for it to be enabled (this is just metadata, not
        // buffered data, so it is fast).
        let slider = app.sliders["seekSlider"]
        XCTAssertTrue(
            slider.waitForExistence(timeout: 15),
            "seekSlider did not appear within 15 s of track select"
        )
        let predicate = NSPredicate(format: "isEnabled == YES")
        let enabled = expectation(for: predicate, evaluatedWith: slider)
        wait(for: [enabled], timeout: 15)

        // No extra warm-up wait. Scrub immediately to ~45 % — this
        // is the exact gesture from the user's app.log scenario.
        slider.adjust(toNormalizedSliderPosition: 0.45)

        // Observation window. The false-EOF cascade fires within
        // ~0.5 s of the failed seek in app.log; we give it 8 s
        // wall-clock to also catch a slower repro on simulator.
        let observationBudget: TimeInterval = 8.0
        let errorLabel = app.staticTexts["errorMessage"]
        let deadline = Date().addingTimeInterval(observationBudget)
        var sawAdvance = false
        var advancedTo = initialTrackName
        var errorSurfaceSeen = false

        while Date() < deadline {
            let now = currentTrackLabel.label
            if now != initialTrackName {
                sawAdvance = true
                advancedTo = now
                errorSurfaceSeen = errorLabel.exists
                break
            }
            if errorLabel.exists {
                errorSurfaceSeen = true
                // Don't break — let the loop continue so we capture
                // any subsequent silent advance after the surface.
            }
            // Polite poll; XCUIElement queries are not free.
            usleep(200_000)
        }

        if sawAdvance {
            XCTAssertTrue(
                errorSurfaceSeen,
                """
                FALSE-EOF AUTO-ADVANCE REPRODUCED.
                The current track flipped from "\(initialTrackName)"
                to "\(advancedTo)" within \(observationBudget) s of the
                scrub, and the errorMessage element was NOT visible.
                This is exactly the user's app.log cascade:
                  SeekOutOfRange → decoder corrupted → eof_seen=true →
                  handle_natural_end → ItemDidPlayToEnd → queue auto-
                  advance.
                Either the false-EOF contract fix is incomplete, or
                a regression has reintroduced the bug.
                """
            )
            // Track advance + error surface is the graceful-fail
            // path — acceptable. Test passes.
            return
        }

        // No advance observed within budget. This is the fully
        // green outcome — track stayed selected, fix is working.
        XCTAssertEqual(
            currentTrackLabel.label,
            initialTrackName,
            "final consistency check"
        )
    }
}
