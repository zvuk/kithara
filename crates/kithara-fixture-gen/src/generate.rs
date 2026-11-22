use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs,
    io::Write as _,
    panic::resume_unwind,
    path::{Path, PathBuf},
    thread,
};

use kithara_platform::time::Instant;

use self::consts::{DISABLE_REMOTE_FIXTURES_ENV, WAIT_REPORTED};
use crate::{
    context::BuildContext,
    graph,
    registry::{AssetBuild, AssetDef},
    store,
};

mod consts {
    use kithara_platform::time::Duration;

    pub(super) const DISABLE_REMOTE_FIXTURES_ENV: &str = "KITHARA_DISABLE_REMOTE_FIXTURES";
    /// A wait on another build's fixture shorter than this is two builds
    /// starting together, not one waiting on the other. Cargo holds a build
    /// script's output until it ends, so the warning a longer wait prints is
    /// the one trace of it the job log gets.
    pub(super) const WAIT_REPORTED: Duration = Duration::from_secs(1);
}

/// Rejects two cases that would produce one accessor, before either is written.
fn resolve(defs: &[&'static AssetDef]) -> Vec<(String, String, &'static AssetDef)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut resolved = Vec::with_capacity(defs.len());
    for def in defs {
        let name = def.accessor_name();
        assert!(
            seen.insert(name.clone()),
            "kithara-test-fixtures: two asset cases share the accessor name `{name}`",
        );
        let id = def.format.map_or_else(
            || store::asset_id(def.func, def.case),
            |format| store::formatted_asset_id(def.func, def.case, &format()),
        );
        resolved.push((name, id, *def));
    }
    resolved.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    resolved
}

/// The accessor names this build must produce again, with every asset built
/// from one of them. A derived asset holds bytes of its dependency, so
/// refreshing a source without its dependents would leave the two disagreeing.
/// Unknown selections are nonfatal because enabled families register different
/// subsets. The small acyclic graph makes repeated dependency closure finite.
fn refreshed(
    resolved: &[(String, String, &'static AssetDef)],
    selection: &store::Refresh,
) -> HashSet<String> {
    let mut names: HashSet<String> = resolved
        .iter()
        .filter(|(name, _, def)| selection.selects(def.func, name))
        .map(|(name, _, _)| name.clone())
        .collect();
    if let store::Refresh::Named(requested) = selection {
        for requested in requested {
            if !resolved
                .iter()
                .any(|(name, _, def)| name == requested || def.func == requested)
            {
                println!(
                    "cargo:warning={} names `{requested}`, which no enabled family registers",
                    store::REFRESH_ENV,
                );
            }
        }
    }
    loop {
        let grown: Vec<String> = resolved
            .iter()
            .filter(|(name, _, def)| {
                !names.contains(name)
                    && def
                        .dependencies
                        .iter()
                        .any(|dependency| names.contains(*dependency))
            })
            .map(|(name, _, _)| name.clone())
            .collect();
        if grown.is_empty() {
            return names;
        }
        names.extend(grown);
    }
}

fn materialize(
    namespace: &Path,
    resolved: &[(String, String, &'static AssetDef)],
    refresh: &HashSet<String>,
) -> HashMap<String, String> {
    let mut unavailable = HashMap::new();
    let nodes: Vec<_> = resolved
        .iter()
        .map(|(name, _, def)| graph::Node {
            name,
            dependencies: def.dependencies,
        })
        .collect();
    let levels =
        graph::levels(&nodes).unwrap_or_else(|error| panic!("kithara-test-fixtures: {error}"));
    let parallelism = thread::available_parallelism().map_or(1, usize::from);

    for level in levels {
        for batch in level.chunks(parallelism) {
            let unavailable_snapshot = &unavailable;
            let results = thread::scope(|scope| {
                batch
                    .iter()
                    .map(|&index| {
                        scope.spawn(move || {
                            materialize_one(
                                namespace,
                                resolved,
                                index,
                                unavailable_snapshot,
                                refresh,
                            )
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|handle| handle.join().unwrap_or_else(|panic| resume_unwind(panic)))
                    .collect::<Vec<_>>()
            });
            unavailable.extend(results.into_iter().flatten());
        }
    }
    unavailable
}

/// Only optional sources without dependencies fetch remotely; optional derived
/// assets are produced locally whenever their dependencies exist. Without hydration,
/// fetching families cannot regenerate, so refresh retains their cached entries.
fn materialize_one(
    namespace: &Path,
    resolved: &[(String, String, &'static AssetDef)],
    index: usize,
    unavailable: &HashMap<String, String>,
    refresh: &HashSet<String>,
) -> Option<(String, String)> {
    let (name, id, def) = &resolved[index];
    let fetches = def.optional && def.dependencies.is_empty();
    let hydration_off = fetches
        && std::env::var_os(DISABLE_REMOTE_FIXTURES_ENV).is_some_and(|value| !value.is_empty());
    let reuse = hydration_off || !refresh.contains(name);
    let report_cache = |phase: &str, decision: &str| {
        if fetches {
            // A diagnostic write failure must not change cache reuse.
            let _ = writeln!(
                std::io::stdout().lock(),
                "cargo:warning=fixture_cache pid={} asset={name} entry={id}.{} \
                 namespace={namespace:?} phase={phase} decision={decision} \
                 refresh={} hydration_disabled={hydration_off}",
                std::process::id(),
                def.ext,
                refresh.contains(name)
            );
        }
    };
    if reuse && store::has_entry(namespace, id, def.ext) {
        report_cache("lookup", "reuse");
        return None;
    }
    if hydration_off {
        report_cache("lookup", "disabled_without_entry");
        return Some((
            name.clone(),
            format!("remote hydration disabled by {DISABLE_REMOTE_FIXTURES_ENV}"),
        ));
    }
    report_cache("lookup", "needs_lock");
    let waiting = Instant::now();
    let _lock = store::lock_entry(namespace, id)
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: lock for `{name}`: {error}"));
    let waited = waiting.elapsed();
    if waited >= WAIT_REPORTED {
        println!(
            "cargo:warning=waited {} s for fixture `{name}`, which another build was producing",
            waited.as_secs()
        );
    }
    if reuse && store::has_entry(namespace, id, def.ext) {
        report_cache("locked_lookup", "reuse");
        return None;
    }
    report_cache("locked_lookup", "produce");
    if let Some((dependency, reason)) = def.dependencies.iter().find_map(|dependency| {
        unavailable
            .get(*dependency)
            .map(|reason| (dependency, reason))
    }) {
        if def.optional {
            return Some((
                name.clone(),
                format!("dependency `{dependency}` unavailable: {reason}"),
            ));
        }
        panic!(
            "kithara-test-fixtures: required fixture `{name}` depends on unavailable `{dependency}`: {reason}"
        );
    }
    let dependency_bytes: Vec<_> = def
        .dependencies
        .iter()
        .map(|dependency| {
            let (_, dependency_id, dependency_def) = resolved
                .iter()
                .find(|(candidate, _, _)| candidate == dependency)
                .unwrap_or_else(|| {
                    panic!(
                        "kithara-test-fixtures: graph accepted missing dependency `{dependency}`"
                    )
                });
            store::read_entry(namespace, dependency_id, dependency_def.ext).unwrap_or_else(|| {
                panic!(
                    "kithara-test-fixtures: dependency `{dependency}` of `{name}` was not materialized"
                )
            })
        })
        .collect();
    let inputs: Vec<_> = dependency_bytes.iter().map(Vec::as_slice).collect();
    let context = BuildContext::new(namespace, id);
    #[cfg(feature = "remote")]
    let bytes = match (def.build)(context, &inputs) {
        AssetBuild::Ready(bytes) => bytes,
        AssetBuild::Unavailable(reason) if def.optional => {
            println!("cargo:warning=optional fixture `{name}` unavailable: {reason}");
            return Some((name.clone(), reason));
        }
        AssetBuild::Unavailable(reason) => {
            panic!("kithara-test-fixtures: required fixture `{name}` unavailable: {reason}")
        }
    };
    #[cfg(not(feature = "remote"))]
    let AssetBuild::Ready(bytes) = (def.build)(context, &inputs);
    assert!(
        !bytes.is_empty(),
        "kithara-test-fixtures: `{name}` produced no bytes"
    );
    store::write_entry(namespace, id, def.ext, &bytes)
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: write `{name}`: {error}"));
    None
}

/// Filesystem-free WASM accessors embed bytes at compile time; native accessors
/// retain store metadata and read entries at runtime.
fn codegen(
    namespace: &Path,
    resolved: &[(String, String, &'static AssetDef)],
    unavailable: &HashMap<String, String>,
    embed_assets: bool,
) -> String {
    let mut out = String::new();
    let mut manifest = String::new();
    let mut by_name = String::new();
    for (name, id, def) in resolved {
        let entry = format!("ENTRY_{}", name.to_uppercase());
        let path = store::entry_path(namespace, id, def.ext);
        let path = path.to_str().unwrap_or_else(|| {
            panic!("kithara-test-fixtures: the store path for `{name}` is not valid UTF-8")
        });
        let relative_path = format!("{}/{id}.{}", store::CACHE_VERSION.trim(), def.ext);
        let content_type = def.content_type;
        let unavailable = unavailable
            .get(name)
            .map_or_else(|| "None".to_owned(), |reason| format!("Some({reason:?})"));
        let (cfg, body) = if def.embed && embed_assets {
            (
                "",
                format!("    crate::asset::Asset::embedded(&{entry}, include_bytes!({path:?}))"),
            )
        } else {
            (
                "#[cfg(not(target_arch = \"wasm32\"))]\n",
                format!(
                    "    static BYTES: ::std::sync::OnceLock<Vec<u8>> = \
                     ::std::sync::OnceLock::new();\n    \
                     crate::asset::Asset::on_disk(&{entry}, &BYTES)"
                ),
            )
        };
        let _ = write!(
            out,
            "static {entry}: crate::asset::AssetEntry = crate::asset::AssetEntry {{\n    \
             name: {name:?},\n    id: {id:?},\n    path: {relative_path:?},\n    \
             content_type: {content_type:?},\n    unavailable: {unavailable},\n}};\n\n\
             {cfg}#[must_use]\n\
             pub fn {name}() -> crate::asset::Asset {{\n{body}\n}}\n\n",
        );
        let _ = writeln!(manifest, "    &{entry},");
        let _ = writeln!(
            by_name,
            "    {}({name:?}, {name}),",
            cfg.replace('\n', "\n        "),
        );
    }
    let _ = write!(
        out,
        "/// Every asset this build materialized.\n\
         pub static MANIFEST: &[&crate::asset::AssetEntry] = &[\n{manifest}];\n\n\
         type AssetLookup = (&'static str, fn() -> crate::asset::Asset);\n\n\
         static BY_NAME: &[AssetLookup] = &[\n{by_name}];\n\n\
         /// The asset this build registered under `name`.\n\
         #[must_use]\n\
         pub fn by_name(name: &str) -> Option<crate::asset::Asset> {{\n    \
         BY_NAME\n        .iter()\n        .find(|(candidate, _)| *candidate == name)\n        \
         .map(|(_, asset)| asset())\n}}\n",
    );
    out
}

/// Materialize every registered asset into the shared store and write one
/// accessor per case into `OUT_DIR/assets.rs`. Called from the build script of
/// the crate the accessors compile into.
/// Only available entries are watched: Cargo treats absent watched files as changed
/// on every build, while hydration environment changes rerun this script. The stamp
/// detects namespace removal; individual watches detect changed entries.
///
/// # Panics
///
/// Panics when a required asset cannot be produced or the store cannot be
/// written: a build script has no other way to fail.
pub fn generate() {
    println!("cargo:rerun-if-env-changed={}", store::STORE_ENV);
    println!("cargo:rerun-if-env-changed={DISABLE_REMOTE_FIXTURES_ENV}");
    println!("cargo:rerun-if-env-changed={}", store::REFRESH_ENV);

    let defs: Vec<&AssetDef> = inventory::iter::<AssetDef>.into_iter().collect();
    assert!(
        !defs.is_empty(),
        "kithara-test-fixtures: the asset registry is empty; either `src/defs` declares no \
         `#[kithara::asset]`, or the registrations were dropped at link time",
    );
    for name in defs.iter().flat_map(|def| def.env) {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let resolved = resolve(&defs);

    let fingerprint = store::CACHE_VERSION.trim();
    let root =
        store::root_from_env().unwrap_or_else(|error| panic!("kithara-test-fixtures: {error}"));
    let namespace = store::namespace(&root, fingerprint);
    let unavailable = materialize(
        &namespace,
        &resolved,
        &refreshed(&resolved, &store::Refresh::requested()),
    );
    for (_, id, def) in resolved
        .iter()
        .filter(|(name, _, _)| !unavailable.contains_key(name))
    {
        println!(
            "cargo:rerun-if-changed={}",
            store::entry_path(&namespace, id, def.ext).display()
        );
    }

    let stamp = store::write_stamp(&namespace, fingerprint)
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: write stamp: {error}"));
    println!("cargo:rerun-if-changed={}", stamp.display());

    let out_dir =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("invariant: cargo always sets OUT_DIR"));
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH")
        .expect("invariant: cargo always sets CARGO_CFG_TARGET_ARCH");
    fs::write(
        out_dir.join("assets.rs"),
        codegen(&namespace, &resolved, &unavailable, target_arch == "wasm32"),
    )
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: write assets.rs: {error}"));
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use kithara_test_utils::kithara;

    use super::{AssetDef, refreshed, resolve, store};

    struct DiagnosticChild(std::process::Child);

    impl Drop for DiagnosticChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn selected_child(name: &str, directory: &std::path::Path) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", name, "--nocapture"])
            .current_dir(directory)
            .env_remove("KITHARA_DISABLE_REMOTE_FIXTURES");
        command
    }

    fn recorded_test(name: &str, case: Option<&str>) -> String {
        let directory = tempfile::tempdir().expect("private diagnostic control directory");
        if let Some(case) = case {
            std::fs::write(directory.path().join("diagnostic-case"), case).expect("select payload");
        }
        let stdout = tempfile::tempfile().expect("capture diagnostic stdout");
        let stderr = tempfile::tempfile().expect("capture diagnostic stderr");
        let mut child = DiagnosticChild(
            selected_child(name, directory.path())
                .stdout(stdout.try_clone().expect("stdout writer"))
                .stderr(stderr.try_clone().expect("stderr writer"))
                .spawn()
                .expect("spawn diagnostic child"),
        );
        let status = child.0.wait().expect("reap diagnostic child");
        let output = read_capture(stdout);
        let errors = read_capture(stderr);
        assert!(status.success(), "{status}: {output}\n{errors}");
        if case.is_some() {
            assert_eq!(
                std::fs::read(directory.path().join("receipt")).expect("payload receipt"),
                b"abc"
            );
        }
        assert!(errors.is_empty(), "{errors}");
        output
    }

    fn read_capture(mut file: std::fs::File) -> String {
        use std::io::{Read as _, Seek as _};
        file.rewind().expect("rewind capture");
        let mut text = String::new();
        file.read_to_string(&mut text).expect("read child capture");
        text
    }

    fn child_payload() {
        let case = match std::fs::read_to_string("diagnostic-case") {
            Ok(case) => case,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("read private child case: {error}"),
        };
        let bytes = match case.as_str() {
            "closed-fetch" => verified_fetch_after_reader_closed(),
            "closed-reuse" => cache_payload(true, false),
            "reuse" => cache_payload(false, false),
            "cache-branches" => cache_payload(false, true),
            "redaction" => redaction_payload(),
            other => panic!("unexpected private child case {other}"),
        };
        assert_eq!(bytes, b"abc");
        let receipt = std::path::Path::new("receipt");
        assert!(!receipt.exists(), "receipt cannot precede the payload");
        std::fs::write(receipt, bytes).expect("record actual returned bytes");
        // Assertions precede exit; closed stdout cannot write a libtest summary.
        std::process::exit(0);
    }

    fn closed_reader_test(name: &str, case: &str) {
        use std::io::{BufRead as _, Write as _};
        child_payload();
        let directory = tempfile::tempdir().expect("private diagnostic control directory");
        std::fs::write(directory.path().join("diagnostic-case"), case).expect("select payload");
        assert!(!directory.path().join("receipt").exists());
        let stderr = tempfile::tempfile().expect("capture child stderr");
        let mut child = DiagnosticChild(
            selected_child(name, directory.path())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(stderr.try_clone().expect("stderr writer"))
                .spawn()
                .expect("spawn closed-reader child"),
        );
        let mut reader =
            std::io::BufReader::new(child.0.stdout.take().expect("only stdout reader"));
        loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).expect("read setup handshake"),
                0,
                "child exited before setup: {}",
                read_capture(stderr.try_clone().expect("stderr reader"))
            );
            if line == "payload-ready\n" {
                break;
            }
        }
        assert!(!directory.path().join("receipt").exists());
        drop(reader);
        child
            .0
            .stdin
            .take()
            .expect("release prepared payload")
            .write_all(b"g")
            .expect("release after reader closure");
        let status = child.0.wait().expect("reap closed-reader child");
        let errors = read_capture(stderr);
        assert!(status.success(), "{status}: {errors}");
        assert!(errors.is_empty(), "{errors}");
        assert_eq!(
            std::fs::read(directory.path().join("receipt")).expect("successful payload receipt"),
            b"abc"
        );
    }

    fn after_reader_closed() {
        use std::io::{Read as _, Write as _};
        let mut output = std::io::stdout().lock();
        writeln!(output, "payload-ready").expect("setup reader is open");
        output.flush().expect("flush setup handshake");
        drop(output);
        let mut release = [0_u8];
        std::io::stdin()
            .read_exact(&mut release)
            .expect("parent releases prepared payload");
        assert_eq!(release, [b'g']);
        let error = writeln!(std::io::stdout().lock(), "closed-reader-control")
            .expect_err("parent closed the only reader");
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }

    fn verified_fetch_after_reader_closed() -> Vec<u8> {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("private loopback server");
        let address = server.server_addr().to_ip().expect("TCP address");
        let url = url::Url::parse(&format!("http://{address}/verified")).expect("local URL");
        after_reader_closed();
        let served = std::thread::spawn(move || {
            let request = server.recv().expect("one local request");
            assert_eq!(request.url(), "/verified");
            assert!(
                !request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("Range"))
            );
            request
                .respond(tiny_http::Response::from_data(b"abc".to_vec()))
                .expect("local response");
        });
        let bytes = crate::remote_file::fetch_verified(
            &url,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            3,
            kithara_platform::time::Duration::from_secs(10),
            kithara_platform::time::Duration::from_secs(2),
        )
        .expect("verified fetch survives diagnostic write failure");
        assert_eq!(bytes, b"abc");
        served.join().expect("response served");
        bytes
    }

    fn cache_payload(closed: bool, branches: bool) -> Vec<u8> {
        fn build(_: crate::context::BuildContext<'_>, _: &[&[u8]]) -> crate::registry::AssetBuild {
            crate::registry::AssetBuild::Ready(b"abc".to_vec())
        }
        fn must_not_build(
            _: crate::context::BuildContext<'_>,
            _: &[&[u8]],
        ) -> crate::registry::AssetBuild {
            panic!("a reusable private entry must not invoke its producer");
        }
        const REUSE: AssetDef = AssetDef {
            dependencies: &[],
            env: &[],
            case: "control",
            content_type: "application/octet-stream",
            ext: "bin",
            func: "diagnostic_cache",
            format: None,
            embed: false,
            optional: true,
            build: must_not_build,
        };
        const PRODUCE: AssetDef = AssetDef { build, ..REUSE };
        let namespace = tempfile::tempdir().expect("private fixture namespace");
        let resolved = resolve(&[if branches { &PRODUCE } else { &REUSE }]);
        let id = &resolved[0].1;
        if !branches {
            store::write_entry(namespace.path(), id, "bin", b"abc")
                .expect("prepare reusable bytes");
        }
        if closed {
            after_reader_closed();
        }
        let unavailable = super::materialize_one(
            namespace.path(),
            &resolved,
            0,
            &std::collections::HashMap::new(),
            &HashSet::new(),
        );
        assert_eq!(unavailable, None);
        if branches {
            assert_eq!(
                super::materialize_one(
                    namespace.path(),
                    &resolved,
                    0,
                    &std::collections::HashMap::new(),
                    &HashSet::new()
                ),
                None
            );
            assert_eq!(
                super::materialize_one(
                    namespace.path(),
                    &resolved,
                    0,
                    &std::collections::HashMap::new(),
                    &HashSet::from([resolved[0].0.clone()])
                ),
                None
            );
        }
        let bytes = store::read_entry(namespace.path(), id, "bin").expect("reused entry");
        assert_eq!(bytes, b"abc");
        bytes
    }

    fn redaction_payload() -> Vec<u8> {
        use kithara_platform::time::Duration;

        use crate::remote_file::{RemoteFileError, fetch_verified};
        let server = tiny_http::Server::http("127.0.0.1:0").expect("private redaction server");
        let address = server.server_addr().to_ip().expect("TCP address");
        let url = url::Url::parse(&format!(
            "http://user-secret:password-secret@{address}/verified?query-secret#fragment-secret"
        ))
        .expect("private URL");
        let served = std::thread::spawn(move || {
            for (status, bytes) in [
                (200, b"abc".as_slice()),
                (200, b"body-secret".as_slice()),
                (403, b"body-secret".as_slice()),
            ] {
                let request = server.recv().expect("local request");
                assert_eq!(request.url(), "/verified?query-secret");
                assert!(
                    request
                        .headers()
                        .iter()
                        .any(|header| header.field.equiv("Authorization"))
                );
                let header = tiny_http::Header::from_bytes(b"X-Private", b"header-secret")
                    .expect("private header");
                request
                    .respond(
                        tiny_http::Response::from_data(bytes)
                            .with_status_code(status)
                            .with_header(header),
                    )
                    .expect("local response");
            }
        });
        let fetch = || {
            fetch_verified(
                &url,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                3,
                Duration::from_secs(10),
                Duration::from_secs(2),
            )
        };
        let bytes = fetch().expect("verified redacted fetch");
        assert_eq!(bytes, b"abc");
        assert!(matches!(
            fetch(),
            Err(RemoteFileError::Length {
                expected: 3,
                received: 11,
                ..
            })
        ));
        assert!(matches!(
            fetch(),
            Err(RemoteFileError::Fetch(
                crate::hls_hydrate::HydrateError::Status { .. }
            ))
        ));
        served.join().expect("all responses served");
        let expired = fetch_verified(&url, "unused", 3, Duration::ZERO, Duration::from_secs(2));
        assert!(matches!(
            expired,
            Err(RemoteFileError::Fetch(
                crate::hls_hydrate::HydrateError::Timeout { .. }
            ))
        ));
        bytes
    }

    fn records(output: &str, kind: &str) -> Vec<String> {
        output
            .lines()
            .filter_map(|line| {
                line.find(&format!("cargo:warning=fixture_{kind} "))
                    .map(|start| format!("{}\n", &line[start..]))
            })
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn verified_fetch_survives_closed_diagnostic_reader() {
        closed_reader_test(
            "generate::tests::verified_fetch_survives_closed_diagnostic_reader",
            "closed-fetch",
        );
    }

    #[kithara::test(native, flash(false))]
    fn cache_reuse_survives_closed_diagnostic_reader() {
        closed_reader_test(
            "generate::tests::cache_reuse_survives_closed_diagnostic_reader",
            "closed-reuse",
        );
    }

    #[kithara::test(native, flash(false))]
    fn diagnostic_records_preserve_partial_resume_with_bounded_output() {
        child_payload();
        let fetch = recorded_test(
            "hls_hydrate::tests::remote_file_resumes_a_stalled_transfer_from_the_byte_it_reached",
            None,
        );
        let cache = recorded_test(
            "generate::tests::diagnostic_records_preserve_partial_resume_with_bounded_output",
            Some("reuse"),
        );
        let fetch = records(&fetch, "fetch");
        let cache = records(&cache, "cache");
        assert_eq!(fetch.len(), 8, "{fetch:?}");
        assert!(fetch.len() <= 9);
        assert_eq!(cache.len(), 1, "{cache:?}");
        assert!(cache.len() <= 2);
        assert!(fetch.iter().chain(&cache).map(String::len).sum::<usize>() <= 8192);
        for fields in [
            "phase=budget_start",
            "timeout_ms=10000 stall_ms=200",
            "phase=request attempt=1 offset=0",
            "phase=response attempt=1 offset=0 status=200",
            "phase=body attempt=1 offset=0 retained_bytes=3 complete=false",
            "phase=request attempt=2 offset=3",
            "phase=response attempt=2 offset=3 status=206",
            "phase=body attempt=2 offset=3 retained_bytes=6 complete=true",
            "phase=complete outcome=verified attempts=2 retained_bytes=6",
        ] {
            assert!(
                fetch.iter().any(|record| record.contains(fields)),
                "missing {fields}: {fetch:?}"
            );
        }
        assert!(fetch.iter().all(|record| record.contains("pid=")
            && record.contains(" at=")
            && record.contains(" url=http://127.0.0.1:")));
        assert!(
            cache[0].contains("phase=lookup decision=reuse refresh=false hydration_disabled=false")
        );
    }

    #[kithara::test(native, flash(false))]
    fn diagnostic_records_retain_cache_production_refresh_and_reuse() {
        child_payload();
        let output = recorded_test(
            "generate::tests::diagnostic_records_retain_cache_production_refresh_and_reuse",
            Some("cache-branches"),
        );
        let cache = records(&output, "cache");
        assert_eq!(cache.len(), 5, "{cache:?}");
        for fields in [
            "phase=lookup decision=needs_lock refresh=false",
            "phase=locked_lookup decision=produce refresh=false",
            "phase=lookup decision=reuse refresh=false",
            "phase=lookup decision=needs_lock refresh=true",
            "phase=locked_lookup decision=produce refresh=true",
        ] {
            assert!(
                cache.iter().any(|record| record.contains(fields)),
                "missing {fields}: {cache:?}"
            );
        }
        assert!(cache.iter().all(
            |record| record.contains("asset=diagnostic_cache_control entry=")
                && record.contains("namespace=")
        ));
    }

    #[kithara::test(native, flash(false))]
    fn diagnostic_records_redact_success_and_failure() {
        child_payload();
        let output = recorded_test(
            "generate::tests::diagnostic_records_redact_success_and_failure",
            Some("redaction"),
        );
        let fetch = records(&output, "fetch");
        assert!(!fetch.is_empty(), "normal reader must retain diagnostics");
        for fields in [
            "outcome=verified",
            "outcome=length_error",
            "outcome=request_error",
            "status=403",
            "phase=budget_expired",
        ] {
            assert!(
                fetch.iter().any(|record| record.contains(fields)),
                "missing {fields}: {fetch:?}"
            );
        }
        for secret in [
            "user-secret",
            "password-secret",
            "query-secret",
            "fragment-secret",
            "header-secret",
            "body-secret",
            "Authorization",
        ] {
            assert!(
                !output.contains(secret),
                "diagnostic output disclosed {secret}"
            );
        }
    }

    #[kithara::test]
    fn refreshing_a_whole_track_refreshes_its_independent_reference() {
        let definitions = inventory::iter::<AssetDef>
            .into_iter()
            .filter(|definition| {
                matches!(
                    definition.accessor_name().as_str(),
                    "drain_tone_default"
                        | "reference_drain_tone_default"
                        | "source_pcm_drain_tone_default"
                        | "analysis_drain_tone_default"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 4);
        let resolved = resolve(&definitions);
        let selected = store::Refresh::Named(vec!["drain_tone_default".to_owned()]);
        let rebuilt = refreshed(&resolved, &selected);
        assert_eq!(rebuilt.len(), 4);
        assert!(rebuilt.contains("drain_tone_default"));
        assert!(rebuilt.contains("reference_drain_tone_default"));
        assert!(rebuilt.contains("source_pcm_drain_tone_default"));
        assert!(rebuilt.contains("analysis_drain_tone_default"));
        let native = store::Refresh::Named(vec!["source_pcm_drain_tone_default".to_owned()]);
        assert_eq!(
            refreshed(&resolved, &native),
            HashSet::from([
                "source_pcm_drain_tone_default".to_owned(),
                "analysis_drain_tone_default".to_owned(),
            ])
        );
        assert!(refreshed(&resolved, &store::Refresh::Nothing).is_empty());
    }
}
