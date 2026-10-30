use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::common::tools::ToolsConfig;
use tracing::{info, warn};

use crate::{
    ci::{config::CiConfig, environment::CacheTrust, process::Process},
    consts,
};

/// The kache one CI job compiles through.
///
/// The job writes a configuration file into a directory of its own, which is
/// also where the daemon keeps its socket and the job's events. The remote is
/// not the job's: kache reads it from its host layer, which the runner writes
/// for its scope, and the job's file outranks that layer key by key only
/// where it names a key.
pub(crate) struct KacheJob {
    directory: PathBuf,
}

/// Where a job keeps what it compiled, and how much of it.
pub(crate) struct LocalStore<'a> {
    pub(crate) directory: &'a Path,
    pub(crate) max_size: &'a str,
}

impl KacheJob {
    pub(crate) fn prepare(directory: PathBuf, store: Option<LocalStore<'_>>) -> Result<Self> {
        let job = Self { directory };
        let runtime = job.directory.join("runtime");
        // A directory left by a job that died under the same name holds a
        // daemon's socket that nothing answers.
        let _ = fs::remove_dir_all(&job.directory);
        fs::create_dir_all(&runtime)
            .with_context(|| format!("creating kache's directory {}", runtime.display()))?;
        let mut cache = toml::Table::new();
        cache.insert("explain_miss".into(), true.into());
        cache.insert("runtime_dir".into(), path_value(&runtime)?);
        cache.insert(
            "daemon_idle_timeout_secs".into(),
            consts::KACHE_DAEMON_IDLE_SECS.into(),
        );
        if let Some(store) = store {
            cache.insert("local_store".into(), path_value(store.directory)?);
            cache.insert("local_max_size".into(), store.max_size.into());
        }
        let mut config = toml::Table::new();
        config.insert("cache".into(), cache.into());
        fs::write(job.config(), toml::to_string(&config)?)
            .with_context(|| format!("writing {}", job.config().display()))?;
        Ok(job)
    }

    /// What the job's compilations are told: the configuration, kache as
    /// the wrapper, and sccache for what kache hands on. cc-rs puts kache in
    /// front of a build script's C compile on its own, seeing it as the Rust
    /// wrapper; a `CMake` build takes it as its compiler launcher, so the
    /// compiler `CMake` records stays the real one whichever job configured the
    /// build directory.
    ///
    /// The compilations see no CI system: kache keeps a GitHub or GitLab job
    /// that is not a push to a protected branch from writing, whatever its
    /// credentials allow, and every branch job of the fork is such a job. The
    /// scope's credentials are what decide where a job writes.
    pub(crate) fn environment(&self, tools: &ToolsConfig) -> [(&'static str, OsString); 7] {
        [
            ("CMAKE_CXX_COMPILER_LAUNCHER", tools.program("kache").into()),
            ("CMAKE_C_COMPILER_LAUNCHER", tools.program("kache").into()),
            ("GITHUB_ACTIONS", OsString::new()),
            ("GITLAB_CI", OsString::new()),
            ("KACHE_CONFIG", self.config().into_os_string()),
            ("KACHE_FALLBACK", tools.program("sccache").into()),
            ("RUSTC_WRAPPER", tools.program("kache").into()),
        ]
    }

    /// Stops the job's daemon, which first stores what it still holds
    /// queued: kache uploads after the compile that produced an entry has
    /// returned. A stop that fails costs the next job those entries, not
    /// this one its outcome.
    pub(crate) fn stop(&self, process: &Process, tools: &ToolsConfig) {
        let mut command = process.command(tools.program("kache"));
        command
            .args(["daemon", "stop"])
            .env("KACHE_CONFIG", self.config());
        if let Err(error) = process.run_command(&mut command, "stop the job's kache daemon") {
            warn!(
                error = format!("{error:#}"),
                "kache stopped before it stored everything this lane compiled"
            );
        }
    }

    pub(crate) fn verify_remote(&self, process: &Process, tools: &ToolsConfig) -> Result<()> {
        let mut command = process.command(tools.program("kache"));
        command
            .env("KACHE_CONFIG", self.config())
            .args(["stats", "--json"]);
        let stats = process.capture_bounded_command(
            &mut command,
            "verify the compiler cache remote",
            Duration::from_secs(60),
            consts::CACHE_EVIDENCE_INPUT_LIMIT,
        )?;
        require_remote(&stats)
    }

    fn config(&self) -> PathBuf {
        self.directory.join("config.toml")
    }
}

pub(crate) fn require_remote(stats: &str) -> Result<()> {
    kithara_ci_bootstrap::require_remote(stats).map_err(Into::into)
}

pub(crate) fn reclaim_local_stores(config: &CiConfig, process: &Process) -> Result<()> {
    let cache = config.host.host_root.join("cache");
    if !cache.is_dir() {
        return Ok(());
    }
    let cache = cache.canonicalize().context("resolving host cache root")?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    for scope in CacheTrust::ALL
        .into_iter()
        .map(CacheTrust::as_str)
        .chain(["bootstrap"])
    {
        let scope = cache.join(scope);
        if !scope.is_dir() {
            continue;
        }
        ensure!(
            !scope.is_symlink(),
            "cache scope is a symlink: {}",
            scope.display()
        );
        // Guest locks do not exclude host writers on the shared volume.
        let native = scope.join(&platform);
        ensure!(
            !native.is_symlink(),
            "cache platform is a symlink: {}",
            native.display()
        );
        let store = native.join(consts::KACHE_DIR);
        if !store.join("index.db").is_file() {
            continue;
        }
        ensure!(
            !store.is_symlink()
                && !store.join("index.db").is_symlink()
                && store.canonicalize()?.starts_with(&cache),
            "compiler store is outside its owned cache path: {}",
            store.display()
        );
        reclaim_store(process, &store, &config.host.sccache_size)?;
    }
    Ok(())
}

fn reclaim_store(process: &Process, store: &Path, max_size: &str) -> Result<()> {
    let directory = tempfile::tempdir().context("creating compiler-cache maintenance runtime")?;
    let job = KacheJob::prepare(
        directory.path().join("kache"),
        Some(LocalStore {
            directory: store,
            max_size,
        }),
    )?;
    let command = |args: &[&str]| {
        let mut command = process.command("kache");
        command
            .env_clear()
            .envs(process.environment_var("PATH").map(|path| ("PATH", path)))
            .env("HOME", directory.path())
            .env("KACHE_CONFIG", job.config())
            .env("KACHE_LOCAL_ONLY", "1")
            .args(args);
        command
    };
    // Automatic GC keeps remote imports for six hours even above the size limit.
    let result = process.capture_bounded_command(
        &mut command(&["gc", "--json"]),
        "collect the local compiler store",
        Duration::from_secs(60),
        65_536,
    );
    let stopped = process.capture_bounded_command(
        &mut command(&["daemon", "stop"]),
        "stop the maintenance kache daemon",
        Duration::from_secs(60),
        65_536,
    );
    if let Err(error) = stopped {
        warn!(%error, "compiler-cache maintenance daemon did not stop");
    }
    let report: serde_json::Value = serde_json::from_str(
        &result.with_context(|| format!("collecting compiler store {}", store.display()))?,
    )?;
    ensure!(
        report["success"] == true,
        "compiler-store collection failed: {report}"
    );
    info!(path = %store.display(), %report, "local compiler store collected");
    Ok(())
}

/// The kache host layer a runner gives every compile of its jobs, named by
/// `KACHE_HOST_CONFIG`: the store compiles land in when no job places them
/// itself, and the remote of the scope `environment` holds the credentials of.
///
/// The remote has to be in a file. kache strips a remote named only in the
/// environment from a daemon it starts itself, because that daemon outlives
/// the compile that started it, and the daemon then uploads nothing. A job's
/// own configuration outranks this layer key by key, so a job that places its
/// store keeps that choice and still shares through this remote. The
/// credentials stay in the environment, which the daemon inherits.
pub(crate) fn host_config(
    environment: &BTreeMap<String, String>,
    store: &Path,
    max_size: &str,
) -> Result<String> {
    let value = |name: &str| {
        environment
            .get(name)
            .with_context(|| format!("the cache environment has no {name}"))
            .map(String::as_str)
    };
    let mut remote = toml::Table::new();
    remote.insert("type".into(), "s3".into());
    remote.insert("bucket".into(), value("SCCACHE_BUCKET")?.into());
    remote.insert("endpoint".into(), value("SCCACHE_ENDPOINT")?.into());
    remote.insert("region".into(), value("SCCACHE_REGION")?.into());
    remote.insert("prefix".into(), consts::KACHE_PREFIX.into());
    let mut cache = toml::Table::new();
    cache.insert("local_store".into(), path_value(store)?);
    cache.insert("local_max_size".into(), max_size.into());
    cache.insert("remote".into(), remote.into());
    let mut config = toml::Table::new();
    config.insert("cache".into(), cache.into());
    Ok(toml::to_string(&config)?)
}

fn path_value(path: &Path) -> Result<toml::Value> {
    Ok(path
        .to_str()
        .with_context(|| format!("{} is not UTF-8", path.display()))?
        .into())
}

impl Drop for KacheJob {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ffi::OsStr};

    use super::*;

    #[test]
    fn ci_requires_a_connected_writable_s3_cache() {
        let ready = serde_json::json!({"remote":"s3://scope/kache", "daemon_connected":true, "daemon_remote_readonly":false});
        assert!(require_remote(&ready.to_string()).is_ok());
        for (field, value) in [
            ("remote", serde_json::Value::Null),
            ("remote", serde_json::json!("filesystem://local")),
            ("daemon_connected", serde_json::json!(false)),
            ("daemon_remote_readonly", serde_json::json!(true)),
            ("daemon_remote_readonly", serde_json::Value::Null),
        ] {
            let mut invalid = ready.clone();
            invalid[field] = value;
            assert!(require_remote(&invalid.to_string()).is_err(), "{field}");
        }
    }

    #[cfg(unix)]
    fn maintenance_process(root: &Path) -> Process {
        let mut paths = vec![root.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Process::new(
            root,
            BTreeMap::from([
                ("PATH".into(), std::env::join_paths(paths).unwrap()),
                ("KACHE_CACHE_DIR".into(), "/unexpected/store".into()),
                ("KACHE_SOCKET_PATH".into(), "/unexpected/daemon.sock".into()),
            ]),
        )
    }

    #[cfg(unix)]
    #[test]
    fn collection_uses_the_host_size_without_the_remote_or_a_jobs_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let observed = directory.path().join("observed.toml");
        let stopped = directory.path().join("stopped");
        let program = directory.path().join("kache");
        crate::testing::install_script(
            &program,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                 'gc --json') test \"$KACHE_LOCAL_ONLY\" = 1 || exit 2; \
                 test -z \"${{KACHE_CACHE_DIR+x}}\" || exit 2; \
                 test -z \"${{KACHE_SOCKET_PATH+x}}\" || exit 2; \
                 cp \"$KACHE_CONFIG\" '{}'; printf '%s' '{{\"success\":true}}' ;;\n\
                 'daemon stop') touch '{}' ;;\n*) exit 3 ;;\nesac\n",
                observed.display(),
                stopped.display(),
            ),
        );
        let process = maintenance_process(directory.path());

        reclaim_store(&process, &store, "2G").unwrap();

        let observed: toml::Table = toml::from_str(&fs::read_to_string(observed).unwrap()).unwrap();
        assert_eq!(observed["cache"]["local_store"].as_str(), store.to_str());
        assert_eq!(observed["cache"]["local_max_size"].as_str(), Some("2G"));
        assert!(!observed["cache"].as_table().unwrap().contains_key("remote"));
        assert!(stopped.is_file());
        assert!(!Path::new(observed["cache"]["runtime_dir"].as_str().unwrap()).exists());
        assert!(store.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn collection_failures_are_reported_after_stopping_the_maintenance_daemon() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("kache");
        let stopped = directory.path().join("stopped");
        let process = maintenance_process(directory.path());
        for result in [
            "exit 7",
            "printf invalid",
            "printf '%s' '{\"success\":false}'",
        ] {
            crate::testing::install_script(
                &program,
                &format!(
                    "#!/bin/sh\ncase \"$*\" in\n'gc --json') {result} ;;\n\
                     'daemon stop') touch '{}' ;;\n*) exit 3 ;;\nesac\n",
                    stopped.display(),
                ),
            );

            assert!(reclaim_store(&process, directory.path(), "2G").is_err());
            assert!(stopped.is_file());
            fs::remove_file(&stopped).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn collection_refuses_a_store_aliased_outside_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let external = directory.path().join("external");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("index.db"), "keep").unwrap();
        let platform = directory.path().join("cache/trusted").join(format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        fs::create_dir_all(&platform).unwrap();
        std::os::unix::fs::symlink(&external, platform.join("kache")).unwrap();
        let mut config = crate::ci::config::fixture();
        config.host.host_root = directory.path().to_path_buf();
        let process =
            Process::recording(directory.path(), crate::ci::process::Recording::default());

        assert!(reclaim_local_stores(&config, &process).is_err());
        assert!(process.recorded().unwrap().steps().is_empty());
        assert_eq!(
            fs::read_to_string(external.join("index.db")).unwrap(),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collection_refuses_an_aliased_platform_even_inside_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let scope = directory.path().join("cache/trusted");
        let foreign = scope.join("foreign/kache");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("index.db"), "keep").unwrap();
        std::os::unix::fs::symlink(
            scope.join("foreign"),
            scope.join(format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )),
        )
        .unwrap();
        let mut config = crate::ci::config::fixture();
        config.host.host_root = directory.path().to_path_buf();
        let process =
            Process::recording(directory.path(), crate::ci::process::Recording::default());

        let error = reclaim_local_stores(&config, &process).unwrap_err();
        assert!(error.to_string().contains("platform"), "{error:#}");
        assert!(process.recorded().unwrap().steps().is_empty());
        assert_eq!(
            fs::read_to_string(foreign.join("index.db")).unwrap(),
            "keep"
        );
    }

    fn written(job: &KacheJob) -> toml::Table {
        toml::from_str(&fs::read_to_string(job.config()).unwrap()).unwrap()
    }

    /// The remote is the runner's to name, in kache's host layer, which a
    /// job's own file outranks key by key: a remote named here would be one
    /// more place the bucket is spelled, and only lanes would see it.
    #[test]
    fn a_jobs_own_file_leaves_the_remote_to_its_runner() {
        let directory = tempfile::tempdir().unwrap();

        let job = KacheJob::prepare(directory.path().join("job"), None).unwrap();

        let config = written(&job);
        let cache = config["cache"].as_table().unwrap();
        assert!(!cache.contains_key("remote"), "{cache:?}");
        assert!(!cache.contains_key("local_store"), "{cache:?}");
    }

    fn store_environment(bucket: &str) -> BTreeMap<String, String> {
        [
            ("SCCACHE_BUCKET", bucket),
            ("SCCACHE_ENDPOINT", "http://kithara-ci-cache:9000"),
            ("SCCACHE_REGION", consts::CACHE_REGION),
            ("AWS_ACCESS_KEY_ID", "key-id"),
            ("AWS_SECRET_ACCESS_KEY", "secret-key"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
    }

    /// kache drops a remote named only in the environment from the daemon it
    /// starts itself, so the daemon would run local-only and upload nothing.
    /// The host layer is a file the daemon reads, so every compile a runner
    /// serves shares through the scope's bucket, whatever command started it.
    #[test]
    fn a_runners_compiles_share_through_its_scopes_bucket() {
        for bucket in ["kithara-trusted", "kithara-review"] {
            let rendered = host_config(
                &store_environment(bucket),
                Path::new("/cache/kache/store"),
                "50G",
            )
            .unwrap();

            let config: toml::Table = toml::from_str(&rendered).unwrap();
            let cache = config["cache"].as_table().unwrap();
            assert_eq!(cache["local_store"].as_str(), Some("/cache/kache/store"));
            assert_eq!(cache["local_max_size"].as_str(), Some("50G"));
            let remote = cache["remote"].as_table().unwrap();
            assert_eq!(remote["type"].as_str(), Some("s3"), "{bucket}");
            assert_eq!(remote["bucket"].as_str(), Some(bucket));
            assert_eq!(
                remote["endpoint"].as_str(),
                Some("http://kithara-ci-cache:9000")
            );
            assert_eq!(remote["region"].as_str(), Some(consts::CACHE_REGION));
            assert_eq!(remote["prefix"].as_str(), Some(consts::KACHE_PREFIX));
        }
    }

    /// The file is readable by every job the runner serves; the credentials
    /// stay in the environment, which the daemon inherits.
    #[test]
    fn a_runners_kache_file_carries_no_credentials() {
        let rendered = host_config(
            &store_environment("kithara-review"),
            Path::new("/cache/kache/store"),
            "50G",
        )
        .unwrap();

        assert!(!rendered.contains("key-id"), "{rendered}");
        assert!(!rendered.contains("secret-key"), "{rendered}");
    }

    #[test]
    fn a_job_without_a_store_keeps_what_it_compiles_to_itself() {
        let directory = tempfile::tempdir().unwrap();
        let store_root = directory.path().join("store");

        let job = KacheJob::prepare(
            directory.path().join("job"),
            Some(LocalStore {
                directory: &store_root,
                max_size: "4G",
            }),
        )
        .unwrap();

        let config = written(&job);
        let cache = config["cache"].as_table().unwrap();
        assert!(!cache.contains_key("remote"));
        assert_eq!(cache["local_store"].as_str(), store_root.to_str());
        assert_eq!(cache["local_max_size"].as_str(), Some("4G"));
        assert_eq!(
            cache["runtime_dir"].as_str().map(PathBuf::from),
            Some(directory.path().join("job/runtime"))
        );
    }

    /// A cancelled job is killed before it can stop its daemon, which kache
    /// detaches from the job; only the daemon itself can end it then.
    #[test]
    fn a_daemon_whose_job_never_stops_it_exits_once_idle() {
        let directory = tempfile::tempdir().unwrap();

        let job = KacheJob::prepare(directory.path().join("job"), None).unwrap();

        let config = written(&job);
        let timeout = config["cache"]["daemon_idle_timeout_secs"].as_integer();
        assert!(timeout.is_some_and(|seconds| seconds > 0), "{timeout:?}");
    }

    /// A `CMake` build reaches kache as its compiler launcher, never as its
    /// compiler: a build directory another job configured with the real
    /// compiler would otherwise see the compiler change, and `CMake` then
    /// deletes its cache and configures without the flags `cmake-rs` passed,
    /// `-fPIC` among them.
    #[test]
    fn a_job_compiles_through_kache_and_hands_on_to_sccache() {
        let directory = tempfile::tempdir().unwrap();
        let tools = ToolsConfig::default();
        let job = KacheJob::prepare(directory.path().join("job"), None).unwrap();

        let environment = job.environment(&tools);

        assert_eq!(
            environment,
            [
                (
                    "CMAKE_CXX_COMPILER_LAUNCHER",
                    OsString::from(tools.program("kache"))
                ),
                (
                    "CMAKE_C_COMPILER_LAUNCHER",
                    OsString::from(tools.program("kache"))
                ),
                ("GITHUB_ACTIONS", OsString::new()),
                ("GITLAB_CI", OsString::new()),
                ("KACHE_CONFIG", job.config().into_os_string()),
                ("KACHE_FALLBACK", OsString::from(tools.program("sccache"))),
                ("RUSTC_WRAPPER", OsString::from(tools.program("kache"))),
            ]
        );
    }

    /// kache keeps a GitHub or GitLab job read-only unless it is a push to a
    /// protected branch, and every branch job of the fork is a push to an
    /// unprotected one. The scope's credentials decide what a job may write.
    #[test]
    fn kache_sees_no_ci_system_whose_policy_would_keep_a_job_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let job = KacheJob::prepare(directory.path().join("job"), None).unwrap();

        let environment = job.environment(&ToolsConfig::default());

        for marker in ["GITHUB_ACTIONS", "GITLAB_CI"] {
            assert_eq!(
                environment
                    .iter()
                    .find(|(name, _)| *name == marker)
                    .map(|(_, value)| value.as_os_str()),
                Some(OsStr::new("")),
                "{marker}"
            );
        }
    }

    #[test]
    fn a_finished_job_leaves_nothing_of_its_own_behind() {
        let directory = tempfile::tempdir().unwrap();
        let own = directory.path().join("job");

        drop(KacheJob::prepare(own.clone(), None).unwrap());

        assert!(!own.exists());
    }
}
