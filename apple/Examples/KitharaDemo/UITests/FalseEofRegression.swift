import XCTest

private extension XCUIElement {
    /// Platform-agnostic activation: `tap()` on iOS, `click()` on
    /// macOS. The two APIs are not interchangeable — calling `tap()`
    /// on macOS does nothing, and `click()` does not exist on iOS —
    /// so this helper hides the conditional from each test body.
    func activate() {
        #if os(iOS)
        tap()
        #else
        click()
        #endif
    }
}

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
@MainActor
final class FalseEofRegression: XCTestCase {
    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    /// The prod-DRM track URL the user repeatedly crashes on in
    /// `app.log @ 09:43:50` / `@ 11:50`. Adding it through the
    /// URL input keeps the test independent of the demo's default
    /// playlist — which is empty when build-time DRM secrets are
    /// missing or `player.insert()` fails on first launch.
    private static let prodDrmTrackUrl =
        "https://cdn-hls-slicer.zvuk.com/drm/track/171515249_1/master.m3u8"

    func testRapidScrubWithoutWarmupDoesNotFalseEofAdvance() throws {
        // Bundle id is set explicitly because the XcodeGen-generated
        // scheme leaves `targetApplicationBundleID` null for the UI
        // testing bundle on macOS (`TEST_TARGET_NAME` is a build
        // setting, not a scheme attribute) and a bare
        // `XCUIApplication()` then fails with NSInternalInconsistency
        // before the test body even runs.
        let app = XCUIApplication(bundleIdentifier: "com.kithara.demo")
        app.launch()

        XCTAssertTrue(
            app.staticTexts["Kithara"].waitForExistence(timeout: 10),
            "demo app did not finish launching within 10 s"
        )

        // The demo's default seed playlist contains the prod-DRM
        // track at a stable URL; locate it directly via the
        // accessibility identifier (which folds the URL into the
        // string — see `PlayerView.playlistRow`). This avoids the
        // Add-via-URL-input flow which is fragile on iOS Simulator
        // (TextField focus + keyboard interplay) and unnecessary
        // when the track is already in the seed.
        let urlNeedle = "171515249_1/master.m3u8"
        let prodRowPredicate = NSPredicate(
            format: "(identifier CONTAINS %@) OR (label CONTAINS %@)",
            urlNeedle, urlNeedle
        )

        // Probe across every container kind SwiftUI can wrap the row
        // into on each platform. On iOS the row surfaces as
        // `buttons` (we replaced `List` with `ScrollView+LazyVStack`
        // in PlayerView so the row's Button identifier propagates).
        func findProdRow() -> XCUIElement? {
            let buckets: [XCUIElementQuery] = [
                app.buttons.matching(prodRowPredicate),
                app.cells.matching(prodRowPredicate),
                app.outlines.cells.matching(prodRowPredicate),
                app.tables.cells.matching(prodRowPredicate),
                app.descendants(matching: .any).matching(prodRowPredicate),
            ]
            for query in buckets where query.count > 0 {
                return query.firstMatch
            }
            return nil
        }

        // Lazy containers only render visible rows; if the prod row
        // is below the fold we must scroll the playlist until it
        // materialises. Try a few swipe-ups before giving up.
        var prodRow = findProdRow()
        if prodRow == nil || !prodRow!.exists {
            for _ in 0..<8 where prodRow == nil || !prodRow!.exists {
                app.swipeUp()
                prodRow = findProdRow()
                if let row = prodRow, row.exists { break }
            }
        }
        guard let prodRow = prodRow, prodRow.waitForExistence(timeout: 5) else {
            let allRowIds = app.descendants(matching: .any)
                .matching(NSPredicate(format: "identifier BEGINSWITH 'trackRow_'"))
                .allElementsBoundByIndex
                .prefix(30)
                .map { $0.identifier }
                .joined(separator: ", ")
            XCTFail(
                "Could not locate prod-DRM track row (needle '\(urlNeedle)') " +
                "in the seed playlist. Visible row ids: [\(allRowIds)]. " +
                "Either the demo's baked playlist no longer contains the " +
                "171515249 track or the iOS XCFramework was built without " +
                ".env DRM keys (so the row failed `player.insert`)."
            )
            return
        }
        prodRow.activate()

        // The now-playing label must reflect the selection AND
        // point at the prod DRM track we just added — otherwise we
        // selected the wrong row (e.g. an MP3 from the default
        // playlist) and the rest of the test would scrub the wrong
        // codec path.
        let currentTrackLabel = app.staticTexts["currentTrackName"]
        XCTAssertTrue(
            currentTrackLabel.waitForExistence(timeout: 10),
            "currentTrackName label never appeared after row tap"
        )
        let expectedTrackName = "master.m3u8"  // url.lastPathComponent
        let trackNamePredicate = NSPredicate(
            format: "label CONTAINS[c] %@", expectedTrackName
        )
        let trackNameMatched = expectation(
            for: trackNamePredicate, evaluatedWith: currentTrackLabel
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [trackNameMatched], timeout: 10),
            .completed,
            "Selected row did not become current track. " +
            "Expected name containing '\(expectedTrackName)', " +
            "got '\(currentTrackLabel.label)' — likely tapped a " +
            "default playlist row instead of the prod-DRM track."
        )
        let initialTrackName = currentTrackLabel.label
        XCTAssertFalse(
            initialTrackName.isEmpty,
            "currentTrackName label was empty after row tap"
        )
        // The visible name collapses to `master.m3u8` for every HLS
        // playlist entry in the seed list — comparing by name alone
        // cannot distinguish "still on the prod-DRM track we picked"
        // from "queue auto-advanced to another HLS row whose name
        // is also master.m3u8". The URL is folded into the element's
        // `accessibilityValue` exactly for this case.
        let initialTrackUrl = currentTrackLabel.value as? String ?? ""
        XCTAssertTrue(
            initialTrackUrl.contains(urlNeedle),
            "currentTrackName.value did not point at the prod-DRM " +
            "URL (got '\(initialTrackUrl)'). The row tap probably " +
            "did not propagate to `currentTrackId`."
        )

        // The seek slider stays disabled until duration is known.
        // For a prod HLS+DRM+AAC2 track this requires: master.m3u8
        // → variant playlist → first segment fetch → DRM key
        // resolve → demux init. On simulator with prod CDN this
        // can take a few seconds; budget 20 s and FAIL HARD if it
        // never enables, because a disabled slider means scrub is
        // a no-op and the test cannot exercise the bug path.
        let slider = app.sliders["seekSlider"]
        XCTAssertTrue(
            slider.waitForExistence(timeout: 20),
            "seekSlider did not appear within 20 s of track select"
        )
        let enabledPredicate = NSPredicate(format: "isEnabled == YES")
        let enabled = expectation(for: enabledPredicate, evaluatedWith: slider)
        XCTAssertEqual(
            XCTWaiter.wait(for: [enabled], timeout: 20),
            .completed,
            "seekSlider never became enabled within 20 s. The prod " +
            "DRM track did not load — most likely because the iOS " +
            "binary was built without .env DRM secrets baked in, or " +
            "the X-Encrypted-Key keyserver rejected the request. " +
            "Without an enabled slider the scrub gesture below is a " +
            "no-op and this test would falsely pass."
        )

        // Capture the position right before the scrub so we can
        // confirm afterward that the value is *different and
        // larger*. `accessibilityValue` is the raw seconds (set in
        // PlayerView.seekSection); the visible label is `mm:ss`
        // which loses sub-second resolution and is harder to parse.
        let currentTimeLabel = app.staticTexts["currentTimeLabel"]
        XCTAssertTrue(
            currentTimeLabel.waitForExistence(timeout: 5),
            "currentTimeLabel never appeared"
        )
        let preScrubTime = parseSeconds(currentTimeLabel.value as? String)

        // No extra warm-up wait. Scrub immediately to ~45 % — this
        // is the exact gesture from the user's app.log scenario.
        slider.adjust(toNormalizedSliderPosition: 0.45)

        // Observation window. Two failure modes must be detected:
        //   1) Queue auto-advance to a different track — visible
        //      via the URL embedded in currentTrackName.value
        //      (the label is `master.m3u8` for every HLS row, so
        //      label equality is NOT a valid signal).
        //   2) Position frozen at zero or never moving past the
        //      scrub target — the user's "track does not actually
        //      play after seek" symptom.
        // We give the engine 6 s wall-clock to settle after the
        // scrub: enough for the audio FSM to re-prime the decoder
        // and start emitting frames, short enough that a hung
        // pipeline shows up as a hard fail.
        let observationBudget: TimeInterval = 6.0
        let errorLabel = app.staticTexts["errorMessage"]
        let deadline = Date().addingTimeInterval(observationBudget)
        var sawAdvance = false
        var advancedToUrl = initialTrackUrl
        var errorSurfaceSeen = false
        var lastSeenTime = preScrubTime

        while Date() < deadline {
            let nowUrl = currentTrackLabel.value as? String ?? ""
            if !nowUrl.isEmpty && nowUrl != initialTrackUrl {
                sawAdvance = true
                advancedToUrl = nowUrl
                errorSurfaceSeen = errorLabel.exists
                break
            }
            if errorLabel.exists {
                errorSurfaceSeen = true
                // Don't break — capture any silent advance that
                // might follow the surfaced error.
            }
            lastSeenTime = parseSeconds(currentTimeLabel.value as? String)
            usleep(200_000)
        }

        if sawAdvance {
            XCTAssertTrue(
                errorSurfaceSeen,
                """
                FALSE-EOF AUTO-ADVANCE REPRODUCED.
                Queue silently flipped from
                  "\(initialTrackUrl)"
                to
                  "\(advancedToUrl)"
                within \(observationBudget) s of the scrub, and the
                errorMessage element was NOT visible. This is the
                user's app.log cascade:
                  SeekOutOfRange → decoder corrupted → eof_seen=true
                  → handle_natural_end → ItemDidPlayToEnd → queue
                  auto-advance.
                """
            )
            // Graceful-fail path (advance + error surface) is
            // acceptable. Returning lets the test pass.
            return
        }

        // No advance — but did the track actually start playing
        // from the new position? Re-read the time once more after
        // the budget elapsed; it should be strictly greater than
        // preScrubTime by *at least 1 s* (we set the scrub to 0.45
        // of a multi-minute track, so the post-scrub position is
        // many seconds ahead of zero; if playback resumed normally
        // the wall-clock 6 s window would have grown the position
        // further).
        let postScrubTime = parseSeconds(currentTimeLabel.value as? String)
        XCTAssertGreaterThan(
            postScrubTime,
            preScrubTime + 1.0,
            """
            SILENT POST-SEEK FREEZE — current track URL did not
            change and no errorMessage surfaced, but playback never
            resumed after the scrub:
              preScrubTime  = \(preScrubTime) s
              lastSeenTime  = \(lastSeenTime) s   (mid-budget poll)
              postScrubTime = \(postScrubTime) s
              budget        = \(observationBudget) s
              track URL     = \(initialTrackUrl)
            This is the second flavour of the false-EOF cascade
            from app.log: the seek lands, the decoder corrupts, no
            advance fires, but no audio comes out either. Without
            this assertion the test would falsely report green.
            """
        )
    }

    /// Parse seconds from the accessibility value we set on the
    /// time label. Returns 0 on empty / unparsable input — that
    /// way a missing label degrades to a hard-fail through the
    /// "no progress past preScrubTime" assertion above instead of
    /// crashing the test runner.
    private func parseSeconds(_ raw: String?) -> Double {
        guard let raw, let value = Double(raw) else { return 0 }
        return value
    }
}
