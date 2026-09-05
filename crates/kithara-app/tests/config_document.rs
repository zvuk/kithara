//! What the shipped document promises the application, end to end.

use std::{fs, path::PathBuf};

use kithara_app::document::Config;
use tempfile::TempDir;

/// Every test here overlays the DRM section with a provider whose cipher key
/// is an inline literal. The shipped providers reference `$KITHARA_...` names
/// that only a build holding credentials resolves, and a test that passes or
/// fails on what the machine happens to export is not a test. The shipped
/// providers' own validity and salt shapes are pinned in `document::policy`,
/// which reads the baked document without expanding it.
const NEUTRAL_DRM: &str = concat!(
    "drm:\n  providers:\n    - name: test\n",
    "      domains: [keys.test]\n      cipher_key: not-a-secret\n",
);

fn tempdir() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn write(dir: &TempDir, contents: &str) -> PathBuf {
    let path = dir.path().join("overlay.yaml");
    fs::write(&path, contents).expect("write the test document");
    path
}

#[kithara::test(native, flash(false))]
fn the_shipped_document_configures_the_application() {
    let dir = tempdir();
    let path = write(&dir, NEUTRAL_DRM);

    let config =
        Config::load(Some(&path), None).expect("the baked document, overlaid with a neutral DRM");

    assert!(
        !config.tracks().is_empty(),
        "the shipped playlist reaches the application"
    );
    assert_eq!(
        config.net().is_insecure,
        Some(true),
        "the shipped document accepts test-server certificates"
    );
    config
        .drm_policy()
        .expect("the shipped providers are valid");
}

#[kithara::test(native, flash(false))]
fn a_file_changes_the_playlist_without_touching_the_rest() {
    let dir = tempdir();
    let path = write(
        &dir,
        &format!("{NEUTRAL_DRM}playlist:\n  tracks: [https://example.test/one.mp3]\n"),
    );

    let config = Config::load(Some(&path), None).expect("the overlay loads");

    assert_eq!(config.tracks(), ["https://example.test/one.mp3"]);
    assert_eq!(
        config.net().is_insecure,
        Some(true),
        "a field the overlay never names keeps its baked value"
    );
}

#[kithara::test(native, flash(false))]
fn the_app_section_reaches_the_config_patch() {
    let dir = tempdir();
    let path = write(
        &dir,
        &format!("{NEUTRAL_DRM}app:\n  eq_bands: 5\n  waveform_max_buckets: 1000\n"),
    );

    let config = Config::load(Some(&path), None).expect("the overlay loads");

    let app = config.app();
    assert_eq!(app.eq_bands, Some(5));
    assert_eq!(app.waveform_max_buckets, Some(1000));
    assert_eq!(
        app.analysis_chunk_seconds, None,
        "a knob the document never names stays unset, so the built value stands"
    );
}

/// The stretch backends' preparation geometry is the deepest nesting a
/// document reaches: `player:` carries a `warp:` section, which carries a
/// `backends:` section, which carries one per compiled engine. Only a build
/// that compiles a backend has the key at all.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test(native, flash(false))]
fn the_document_reaches_the_stretch_backend_geometry() {
    const WARP_BACKENDS: &str = concat!(
        "player:\n  warp:\n    backends:\n",
        "      signalsmith:\n        block_frames: 512\n        interval_frames: 16\n",
        "      bungee:\n        log2_synthesis_hop_adjust: -2\n",
    );

    let dir = tempdir();
    let path = write(&dir, &format!("{NEUTRAL_DRM}{WARP_BACKENDS}"));

    let config = Config::load(Some(&path), None).expect("the overlay loads");

    let backends = config.player().warp.backends;
    assert_eq!(
        backends.signalsmith.block_frames,
        std::num::NonZeroUsize::new(512)
    );
    assert_eq!(
        backends.signalsmith.interval_frames,
        std::num::NonZeroUsize::new(16)
    );
    assert_eq!(backends.bungee.log2_synthesis_hop_adjust, Some(-2));
}

#[kithara::test(native, flash(false))]
fn an_unknown_app_knob_is_refused() {
    let dir = tempdir();
    let path = write(&dir, &format!("{NEUTRAL_DRM}app:\n  eq_band: 5\n"));

    let error = Config::load(Some(&path), None).expect_err("a typo must not pass silently");

    assert!(error.to_string().contains("eq_band"), "{error}");
}
