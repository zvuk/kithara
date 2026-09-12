#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::{assets, store};
use kithara_test_utils::kithara;

#[kithara::test(native, flash(false))]
fn the_registry_is_not_empty() {
    assert!(
        !assets::MANIFEST.is_empty(),
        "no assets were registered; the build script must fail before this test can",
    );
}

#[kithara::test(native, flash(false))]
fn every_manifest_entry_is_materialized_or_explicitly_unavailable() {
    let origin = std::env::var_os(store::ORIGIN_ENV);
    for entry in assets::MANIFEST {
        assert!(Path::new(entry.path).is_relative());
        if let Some(reason) = entry.unavailable {
            assert!(
                !reason.is_empty(),
                "asset {} has an empty failure",
                entry.name
            );
            if origin.is_none() {
                let path = store::runtime_root()
                    .expect("fixture root")
                    .join(entry.path);
                assert!(
                    !path.exists(),
                    "unavailable asset {} unexpectedly exists at {}",
                    entry.name,
                    entry.path,
                );
            }
            continue;
        }
        if origin.is_some() {
            continue;
        }
        let path = store::runtime_root()
            .expect("fixture root")
            .join(entry.path);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("asset {} at {}: {error}", entry.name, entry.path));
        assert!(!bytes.is_empty(), "asset {} is empty", entry.name);
    }
}

#[kithara::test(native, flash(false))]
fn every_id_is_unique_and_derived_from_the_accessor_name() {
    let mut seen = HashSet::new();
    for entry in assets::MANIFEST {
        assert!(seen.insert(entry.id), "duplicate asset id {}", entry.id);
        assert!(
            entry.path.contains(entry.id),
            "asset {} does not live at its own id",
            entry.name,
        );
    }
}

#[kithara::test(native, flash(false))]
fn the_pilot_asset_is_a_riff_wave_of_the_declared_length() {
    const HEADER_BYTES: usize = 44;
    const FRAMES: usize = 264_600;
    const CHANNELS: usize = 2;
    const SAMPLE_BYTES: usize = 2;

    let asset = assets::sine_wav_a440_6s();
    let bytes = asset.bytes();

    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(bytes.len(), HEADER_BYTES + FRAMES * CHANNELS * SAMPLE_BYTES);
    assert_eq!(asset.entry().content_type, "audio/wav");
    assert_eq!(asset.path(), Some(asset_path(&asset)));
}

fn asset_path(asset: &kithara_test_fixtures::asset::Asset) -> PathBuf {
    let relative = assets::MANIFEST
        .iter()
        .find(|entry| entry.id == asset.entry().id)
        .map(|entry| entry.path)
        .unwrap_or_else(|| panic!("asset {} is missing from the manifest", asset.entry().id));
    store::runtime_root().expect("fixture root").join(relative)
}

#[kithara::test(native, flash(false))]
fn the_accessor_serves_what_the_store_holds() {
    let asset = assets::sine_wav_a440_6s();
    let stored = std::fs::read(asset.path().expect("BUG: the pilot asset lives on disk"))
        .expect("read the stored asset");

    assert_eq!(asset.bytes(), stored.as_slice());
}

#[kithara::test(native, flash(false))]
fn the_namespace_uses_the_explicit_cache_revision() {
    let asset = assets::sine_wav_a440_6s();
    let path = asset.path().expect("BUG: the pilot asset lives on disk");
    let namespace = path.parent().expect("an entry lives inside a namespace");
    let fingerprint = namespace
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the namespace is named by the fingerprint");

    assert_eq!(fingerprint, store::CACHE_VERSION.trim());
}

#[kithara::test(native, flash(false))]
fn an_embed_marked_asset_has_a_native_store_path() {
    let asset = assets::marked_sine_wav_a440_6s();
    assert_eq!(
        asset.path(),
        Some(asset_path(&asset)),
        "an embed-marked asset must use the store on native targets",
    );
}

#[kithara::test(native, flash(false))]
fn an_embed_marked_asset_reads_the_bytes_that_were_stored() {
    let asset = assets::marked_sine_wav_a440_6s();
    let bytes = asset.bytes();
    let stored = std::fs::read(
        asset
            .path()
            .expect("BUG: the embed-marked asset lives on disk"),
    )
    .expect("read the stored asset");

    assert_eq!(bytes, stored.as_slice());
}

#[kithara::test(native, flash(false))]
fn the_encoded_pilot_is_an_mpeg_audio_stream() {
    const ID3_TAG: &[u8; 3] = b"ID3";
    const MPEG_SYNC: u8 = 0xFF;

    let asset = assets::sine_mp3_a440_2s();
    let bytes = asset.bytes();

    assert_eq!(asset.entry().content_type, "audio/mpeg");
    assert!(
        bytes.starts_with(ID3_TAG) || bytes.first() == Some(&MPEG_SYNC),
        "expected an MPEG audio stream, got {:?}",
        &bytes[..bytes.len().min(ID3_TAG.len())],
    );
    assert!(bytes.len() > ID3_TAG.len());
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod process_config {
    use std::{
        collections::HashMap,
        net::TcpListener,
        process::Command,
        thread::{self, JoinHandle},
    };

    use kithara_platform::time::{Duration, Instant};
    use tiny_http::{Response, Server, StatusCode as TinyStatus};

    use super::*;

    const CHILD_MODE: &str = "KITHARA_FIXTURE_STORE_CHILD";
    const RELOCATED_BYTES: &[u8] = b"relocated fixture bytes";
    const ORIGIN_BYTES: &[u8] = b"from-http-origin";

    fn check_runtime_store(mode: &str, root: Option<&Path>) {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "process_config::runtime_store_child",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode);
        match root {
            Some(root) => command.env(store::STORE_ENV, root),
            None => command.env_remove(store::STORE_ENV),
        };
        let output = command.output().expect("run fixture store subprocess");
        assert!(
            output.status.success(),
            "runtime store {mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[kithara::test(native, flash(false))]
    fn runtime_override_relocates_bytes_and_paths_together() {
        let root = tempfile::tempdir().expect("relocated store");
        for asset in [
            assets::sine_wav_a440_6s(),
            assets::marked_sine_wav_a440_6s(),
        ] {
            let path = root.path().join(asset.entry().path);
            std::fs::create_dir_all(path.parent().expect("cache namespace"))
                .expect("create namespace");
            std::fs::write(path, RELOCATED_BYTES).expect("stage fixture");
        }
        check_runtime_store("present", Some(root.path()));
    }

    #[kithara::test(native, flash(false))]
    fn missing_runtime_entry_does_not_use_the_build_store() {
        assert!(!assets::sine_wav_a440_6s().bytes().is_empty());
        let root = tempfile::tempdir().expect("empty runtime store");
        check_runtime_store("missing", Some(root.path()));
    }

    #[kithara::test(native, flash(false))]
    fn relative_runtime_override_is_rejected() {
        check_runtime_store("relative", Some(Path::new("relative-store")));
    }

    #[kithara::test(native, flash(false))]
    fn absent_runtime_override_uses_the_build_store() {
        check_runtime_store("default", None);
    }

    #[kithara::test(native, flash(false))]
    fn http_origin_is_the_record_source_and_the_cache_survives_a_process() {
        let relative = assets::sine_wav_a440_6s().entry().path;
        let cache = tempfile::tempdir().expect("origin cache");
        let server = OriginServer::serve(
            HashMap::from([(
                format!("/store/{relative}"),
                (200_u16, ORIGIN_BYTES.to_vec()),
            )]),
            1,
        );
        let origin = server.url.clone();
        check_http_origin("http-fetch", cache.path(), Some(&origin));
        assert_eq!(server.finish().values().sum::<usize>(), 1);
        check_http_origin("http-replay", cache.path(), Some(&origin));
    }

    #[kithara::test(native, flash(false))]
    fn http_origin_requires_the_cache_directory() {
        check_http_origin(
            "http-missing-cache",
            Path::new("/unused"),
            Some("http://127.0.0.1:1"),
        );
    }

    #[kithara::test(native, flash(false))]
    fn http_origin_rejects_a_non_loopback_url() {
        let cache = tempfile::tempdir().expect("origin cache");
        check_http_origin(
            "http-remote-host",
            cache.path(),
            Some("http://example.com:80"),
        );
    }

    #[kithara::test(native, flash(false))]
    fn unreachable_http_origin_fails_before_a_suite_timeout() {
        let cache = tempfile::tempdir().expect("origin cache");
        let listener = TcpListener::bind("127.0.0.1:0").expect("closed origin port");
        let origin = format!("http://{}", listener.local_addr().expect("bound address"));
        drop(listener);
        let started = Instant::now();
        check_http_origin("http-unreachable", cache.path(), Some(&origin));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an unreachable origin must fail immediately, not hang the suite"
        );
    }

    #[kithara::test(native, flash(false))]
    fn store_file_rejects_paths_that_escape_the_root() {
        assert!(store::file(Path::new("../secret")).is_err());
        assert!(store::file(Path::new("/etc/passwd")).is_err());
    }

    fn check_http_origin(mode: &str, cache: &Path, origin: Option<&str>) {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "process_config::runtime_store_child",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode)
            .env(store::STORE_ENV, cache);
        match origin {
            Some(origin) => {
                command.env(store::ORIGIN_ENV, origin);
            }
            None => {
                command.env_remove(store::ORIGIN_ENV);
            }
        }
        if mode == "http-missing-cache" {
            command.env_remove(store::STORE_ENV);
        }
        let output = command.output().expect("run fixture origin subprocess");
        assert!(
            output.status.success(),
            "http origin {mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    struct OriginServer {
        handle: JoinHandle<HashMap<String, usize>>,
        url: String,
    }

    impl OriginServer {
        fn serve(routes: HashMap<String, (u16, Vec<u8>)>, requests: usize) -> Self {
            let server = Server::http("127.0.0.1:0").expect("bind origin");
            let url = format!("http://{}", server.server_addr());
            let handle = thread::spawn(move || {
                let mut counts = HashMap::new();
                for _ in 0..requests {
                    let Some(request) = server
                        .recv_timeout(Duration::from_secs(2))
                        .expect("receive origin request")
                    else {
                        break;
                    };
                    let path = request
                        .url()
                        .split('?')
                        .next()
                        .unwrap_or_else(|| request.url())
                        .to_owned();
                    *counts.entry(path.clone()).or_insert(0) += 1;
                    let (status, body) = routes.get(&path).cloned().unwrap_or((404, Vec::new()));
                    request
                        .respond(Response::from_data(body).with_status_code(TinyStatus(status)))
                        .expect("respond");
                }
                counts
            });
            Self { handle, url }
        }

        fn finish(self) -> HashMap<String, usize> {
            self.handle.join().expect("origin thread")
        }
    }

    #[kithara::test(native, flash(false))]
    #[ignore = "subprocess entrypoint"]
    fn runtime_store_child() {
        use kithara_test_fixtures::asset::AssetError;

        let asset = assets::sine_wav_a440_6s();
        let embed_marked = assets::marked_sine_wav_a440_6s();
        let mode = std::env::var(CHILD_MODE).expect("child mode");
        match mode.as_str() {
            "present" => {
                let root = store::root_from_env().expect("runtime override");
                for candidate in [&asset, &embed_marked] {
                    let path = root.join(candidate.entry().path);
                    assert_eq!(candidate.path(), Some(path.clone()));
                    assert_eq!(
                        candidate.try_bytes().expect("relocated asset"),
                        RELOCATED_BYTES
                    );
                    assert_eq!(
                        std::fs::read(path).expect("relocated path"),
                        candidate.bytes()
                    );
                }
            }
            "missing" => {
                let root = store::root_from_env().expect("runtime override");
                for candidate in [&asset, &embed_marked] {
                    let expected = root.join(candidate.entry().path);
                    assert!(
                        matches!(candidate.try_bytes(), Err(AssetError::Read { path, source, .. })
                    if path == expected && source.kind() == std::io::ErrorKind::NotFound)
                    );
                    assert_eq!(candidate.path(), Some(expected));
                }
            }
            "relative" => {
                for candidate in [&asset, &embed_marked] {
                    assert!(
                        matches!(candidate.try_bytes(), Err(AssetError::Store(source))
                    if source.kind() == std::io::ErrorKind::InvalidInput)
                    );
                }
            }
            "default" => {
                for candidate in [&asset, &embed_marked] {
                    assert_eq!(
                        candidate.path(),
                        Some(Path::new(env!("KITHARA_FIXTURE_CACHE")).join(candidate.entry().path)),
                    );
                    assert!(!candidate.bytes().is_empty());
                }
            }
            "http-fetch" | "http-replay" => {
                let relative = Path::new(asset.entry().path);
                let path = store::file(relative).expect("origin record");
                let bytes = std::fs::read(&path).expect("cached origin record");
                assert_eq!(bytes, ORIGIN_BYTES);
                assert_ne!(&bytes[..bytes.len().min(4)], b"RIFF");
            }
            "http-missing-cache" => {
                let error = store::file(Path::new(asset.entry().path)).expect_err("cache required");
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }
            "http-remote-host" => {
                let error = store::file(Path::new(asset.entry().path)).expect_err("loopback only");
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }
            "http-unreachable" => {
                let origin = std::env::var(store::ORIGIN_ENV).expect("origin");
                let error = store::file(Path::new(asset.entry().path)).expect_err("dead origin");
                let message = error.to_string();
                assert!(
                    message.contains(&origin),
                    "unreachable origin must name {origin}: {message}"
                );
                assert!(
                    message.contains("unreachable") && message.contains("adb reverse"),
                    "unreachable origin must name the reverse mapping: {message}"
                );
            }
            other => panic!("unknown child mode: {other}"),
        }
    }
}
