use std::{fs, path::PathBuf};

use anyhow::Result;
use kithara_devtools::common::tools::ToolsConfig;
use serde_json::{Value, json};
use tracing::warn;
pub(in crate::ci) use xtask::ci::cache_report::sccache_count;
use xtask::ci::cache_report::{self, observations};

use crate::ci::process::Process;

/// Clear at lane entry, including runs that fail before reaching cache reporting.
pub(in crate::ci) fn clear(process: &Process, lane: &str) {
    let Ok(path) = report_path(process, lane) else {
        return;
    };
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn!("cannot remove stale cache evidence");
    }
}

pub(in crate::ci) fn report_path(process: &Process, lane: &str) -> Result<PathBuf> {
    Ok(cache_report::path(process.root(), lane, |name| {
        process.environment_var(name)
    })?)
}

pub(super) fn record(phase: &str) -> Result<()> {
    let root = std::env::current_dir()?;
    let tools = ToolsConfig::load(&root)?;
    let process = Process::new(&root, std::collections::BTreeMap::new());
    let primary = crate::ci::kache::statistics(&process, &tools);
    let fallback = process.capture_bounded(
        tools.program("sccache"),
        &["--show-stats"],
        "sccache statistics",
        cache_report::INPUT_LIMIT,
    );
    retain(&process, &tools, &primary, &fallback, phase)
}

/// Retain only typed observations before the caller stops its cache daemon.
pub(in crate::ci) fn retain(
    process: &Process,
    tools: &ToolsConfig,
    primary: &Result<String>,
    fallback: &Result<String>,
    lane: &str,
) -> Result<()> {
    let full = process.capture_bounded(
        tools.program("kache"),
        &["stats", "--full", "--json"],
        "full kache statistics",
        cache_report::INPUT_LIMIT,
    );
    let explain = process.capture_bounded(
        tools.program("kache"),
        &["explain", "--json"],
        "kache miss explanations",
        cache_report::INPUT_LIMIT,
    );
    let mut report = observations(
        parsed(primary),
        parsed(&full),
        parsed(&explain),
        fallback.as_ref().ok().map(String::as_str),
    );
    let revision = process
        .capture_bounded(
            tools.program("git"),
            &["rev-parse", "HEAD"],
            "cache evidence revision",
            64,
        )
        .ok();
    report["revision"] = json!(revision.as_deref());
    report["steps"] = json!(process.timing_evidence());
    report["key_history"] = history(process).unwrap_or(Value::Null);
    report["transfer_history"] = process
        .environment_path("KACHE_RUNTIME_DIR")
        .and_then(|runtime| cache_report::transfer_history(&runtime))
        .unwrap_or(Value::Null);
    Ok(cache_report::write(
        process.root(),
        lane,
        |name| process.environment_var(name),
        report,
    )?)
}

fn parsed(input: &Result<String>) -> Option<Value> {
    cache_report::parse(input.as_ref().ok().map(String::as_str))
}

fn history(process: &Process) -> Option<Value> {
    let runtime = process
        .environment_path("KACHE_RUNTIME_DIR")
        .filter(|p| !p.as_os_str().is_empty())?;
    cache_report::history(&runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_reports_retain_each_owned_runtime() {
        let root = tempfile::tempdir().unwrap();
        for ci in ["CI_JOB_ID", "GITHUB_RUN_ID"] {
            for phase in ["bootstrap", "bootstrap-driver", "bootstrap-refresh"] {
                assert!(cache_report::path(root.path(), phase, |_| None).is_err());
                let record = |runtime: &str, misses| {
                    let environment = |name: &str| match name {
                        "KACHE_RUNTIME_DIR" => Some(runtime.into()),
                        "GITHUB_RUN_ATTEMPT" => Some("2".into()),
                        name if name == ci => Some("17".into()),
                        _ => None,
                    };
                    cache_report::write(
                        root.path(),
                        phase,
                        environment,
                        observations(Some(json!({"misses": misses})), None, None, None),
                    )
                    .unwrap();
                    cache_report::path(root.path(), phase, environment).unwrap()
                };
                let compiled = record("/private/first-runtime", 7);
                let idle = record("/private/second-runtime", 0);
                assert_ne!(compiled, idle, "{ci}: {phase}");
                assert_eq!(record("/private/first-runtime", 9), compiled);
                for (path, misses) in [(compiled, 9), (idle, 0)] {
                    let bytes = fs::read(&path).unwrap();
                    let report: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(report["lane"], phase);
                    assert_eq!(report["kache"]["counts"]["misses"], misses);
                    let digest = report["job"]["cache_runtime_sha256"].as_str().unwrap();
                    assert_eq!(digest.len(), 64);
                    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
                    assert!(!String::from_utf8(bytes).unwrap().contains("/private/"));
                    let name = path.file_name().unwrap().to_str().unwrap();
                    assert!(name.contains("-bootstrap"));
                    assert!(name.ends_with(&format!("-{digest}.json")));
                }
            }
        }
    }

    #[test]
    fn cleared_ci_identities_select_local_evidence() {
        let root = tempfile::tempdir().unwrap();
        let process = Process::new(
            root.path(),
            std::collections::BTreeMap::from([
                ("CI_JOB_ID".into(), "".into()),
                ("GITHUB_RUN_ID".into(), "".into()),
            ]),
        );
        assert_eq!(
            report_path(&process, "test").unwrap(),
            root.path().join(format!(
                ".ci-artifacts/cache/local-{}-test.json",
                std::process::id()
            ))
        );
    }

    #[test]
    fn cache_evidence_drops_secrets_even_in_known_fields() {
        let secret = "AWS_SECRET_ACCESS_KEY=synthetic-secret";
        let input = json!({"schema_version": 1, "local_hits": 4, "misses": secret, "errors": secret,
            "remote": secret, "disk": {"store_bytes": secret},
            "summary": {"errors": 2, "store_failures": secret},
            "causes": [{"why": format!("compiled but not cached: {secret} (os error 28)"), "fix": secret, "count": 3, "compile_ms": secret, "failure": true}],
            "errors_detail": [{"message": secret}], "env": {"AWS_SECRET_ACCESS_KEY": secret}});
        let report = observations(
            Some(input.clone()),
            Some(input.clone()),
            Some(input),
            Some("Cache hits 9\nCache misses 8\nCache write errors secret\n"),
        );
        assert!(!report.to_string().contains(secret));
        assert_eq!(report["kache"]["counts"]["local_hits"], 4);
        assert_eq!(report["sccache"]["hits"], 9);
        assert_eq!(report["sccache"]["misses"], 8);
        assert_eq!(report["kache"]["explain"]["causes"][0]["os_error_code"], 28);
        assert_eq!(
            report["kache"]["explain"]["causes"][0]["category"],
            "store_failed"
        );
        assert!(report["kache"]["counts"]["misses"].is_null());
        assert!(report["kache"]["counts"]["errors"].is_null());
        assert!(report["kache"]["disk"]["store_bytes"].is_null());
        assert!(report["sccache"]["write_errors"].is_null());
    }

    #[test]
    fn daemon_errors_remain_visible_without_full_inventory() {
        let report = observations(Some(json!({"errors": 3})), None, None, None);
        assert_eq!(report["kache"]["counts"]["errors"], 3);
        assert!(report["kache"]["summary"]["errors"].is_null());
        assert!(report["kache"]["summary"]["store_failures"].is_null());
    }

    #[test]
    fn sqlite_store_contention_is_visible_without_the_error_message() {
        for (message, expected) in [
            ("database is locked", "store_sqlite_locked"),
            ("database table is locked", "store_sqlite_locked"),
            (
                "secret path/database is locked: Permission denied (os error 13)",
                "store_failed",
            ),
            ("unrecognized failure", "store_failed"),
        ] {
            let report = observations(
                None,
                None,
                Some(json!({"causes": [{
                    "why": format!("compiled but not cached: secret path: {message}"),
                    "failure": true
                }]})),
                None,
            );
            assert_eq!(
                report["kache"]["explain"]["causes"][0]["category"],
                expected
            );
            assert_eq!(report["kache"]["explain"]["causes"][0]["failure"], true);
            assert!(!report.to_string().contains("secret"));
        }
    }

    #[test]
    fn unavailable_cache_observations_stay_unknown() {
        let report = observations(None, None, None, None);
        assert!(report["kache"]["counts"]["misses"].is_null());
        assert!(report["kache"]["counts"]["errors"].is_null());
        assert!(report["kache"]["explain"]["counts"]["earlier_misses"].is_null());
        assert!(report["kache"]["gc"]["bytes_held"].is_null());
        assert!(report["actual_linker"].is_null());
        assert!(report["queue_ms"]["runner"].is_null());
        assert!(parsed(&Ok("x".repeat(cache_report::INPUT_LIMIT + 1))).is_none());
    }

    #[test]
    fn costly_units_keep_measured_costs_without_compiler_inputs() {
        let input = json!({"top_misses": [{"crate_name": "kithara_dsp_tests", "cache_key": "a".repeat(64),
            "compile_time_ms": 120000, "elapsed_ms": 120500, "command": "AWS_SECRET_ACCESS_KEY=secret"},
            {"crate_name": "AWS_SECRET_ACCESS_KEY=secret", "compile_time_ms": "secret"}]});
        let report = observations(None, Some(input), None, None);
        assert_eq!(report["kache"]["top_misses"][0]["compile_time_ms"], 120000);
        assert_eq!(
            report["kache"]["top_misses"][0]["crate_name"],
            "kithara_dsp_tests"
        );
        assert!(report["kache"]["top_misses"][1]["crate_name"].is_null());
        assert!(!report.to_string().contains("secret"));
    }

    #[test]
    fn bootstrap_history_uses_the_effective_runtime_without_a_config_file() {
        let directory = tempfile::tempdir().unwrap();
        let process = Process::new(
            directory.path(),
            std::collections::BTreeMap::from([
                ("KACHE_RUNTIME_DIR".into(), directory.path().into()),
                ("KACHE_CONFIG".into(), "".into()),
            ]),
        );
        for (error, expected, category) in [
            (
                Some("secret path: Permission denied (os error 13)"),
                Some(13),
                Some("store_failed"),
            ),
            (
                Some("secret path: unknown error (os error -1)"),
                Some(-1),
                Some("store_failed"),
            ),
            (
                Some("secret path: disk full (os error 28)"),
                Some(28),
                Some("store_failed"),
            ),
            (
                Some("secret path (os error 2147483648)"),
                None,
                Some("store_failed"),
            ),
            (
                Some("secret path (os error invalid)"),
                None,
                Some("store_failed"),
            ),
            (Some("secret path"), None, Some("store_failed")),
            (
                Some("secret path: database is locked"),
                None,
                Some("store_sqlite_locked"),
            ),
            (
                Some("secret path: database table is locked"),
                None,
                Some("store_sqlite_locked"),
            ),
            (Some(""), None, None),
            (None, None, None),
        ] {
            fs::write(
                directory.path().join("events.jsonl"),
                json!({"store_error": error}).to_string(),
            )
            .unwrap();
            let retained = history(&process).expect("bootstrap runtime history");
            assert_eq!(
                retained["records"][0]["store_os_error_code"],
                json!(expected)
            );
            assert_eq!(
                retained["records"][0]["store_failed"],
                json!(error.map(|error| !error.is_empty()))
            );
            assert_eq!(
                retained["records"][0]["store_failure_category"],
                json!(category)
            );
            assert!(!retained.to_string().contains("secret"));
        }
    }

    #[test]
    fn transfer_history_bounds_failures_and_preserves_unknown_data() {
        let root = tempfile::tempdir().unwrap();
        assert!(cache_report::transfer_history(root.path()).is_none());
        let path = root.path().join("transfers.jsonl");
        let known = json!({"ok": false, "direction": "download", "outcome": "error",
            "cache_key": "a".repeat(64), "elapsed_ms": 12});
        let unknown = json!({"ok": false, "direction": "secret", "outcome": "secret",
            "cache_key": "secret", "request_ms": "secret", "url": "secret"});
        let ignored = [
            json!({"ok": true}),
            json!({"ok": false, "outcome": "not_found"}),
            json!({"ok": false, "outcome": "cancelled"}),
            json!({"ok": false, "outcome": "skipped"}),
        ];
        let mut lines = format!("{unknown}\n");
        for event in ignored {
            lines.push_str(&format!("{event}\n"));
        }
        lines.push_str(&format!("{known}\n").repeat(16));
        lines.push_str("invalid\n");
        fs::write(&path, lines).unwrap();
        let retained = cache_report::transfer_history(root.path()).unwrap();
        assert_eq!(retained["observed_failures"], 17);
        assert_eq!(retained["records"].as_array().unwrap().len(), 16);
        assert_eq!(retained["invalid_records"], 1);
        assert_eq!(retained["truncated"], true);
        assert!(retained["records"][0]["outcome"].is_null());
        assert!(retained["records"][0]["cache_key"].is_null());
        assert!(retained["records"][0]["request_ms"].is_null());
        assert_eq!(retained["records"][1]["elapsed_ms"], 12);
        assert!(!retained.to_string().contains("secret"));
        fs::write(&path, b"\n".repeat(cache_report::INPUT_LIMIT + 1)).unwrap();
        let retained = cache_report::transfer_history(root.path()).unwrap();
        assert_eq!(retained["truncated"], true);
        assert_eq!(retained["observed_failures"], 0);
    }

    #[test]
    fn runtime_history_survives_cleanup_without_raw_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let job =
            crate::ci::kache::KacheJob::prepare(&directory.path().join("kache"), None).unwrap();
        let tools = ToolsConfig::default();
        let bin = directory.path().join("bin");
        crate::testing::install_double(&bin, "kache");
        crate::testing::install_double(&bin, "git");
        let mut vars: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString> = job
            .environment(&tools)
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect();
        vars.insert("PATH".into(), bin.into());
        vars.insert("CI_JOB_ID".into(), "123".into());
        vars.insert("GITHUB_JOB".into(), "AWS_SECRET_ACCESS_KEY=secret".into());
        vars.insert(
            "GITHUB_RUN_ID".into(),
            "AWS_SECRET_ACCESS_KEY=secret".into(),
        );
        vars.insert("KITHARA_TEST_STDOUT".into(), "a".repeat(40).into());
        let process = Process::new(directory.path(), vars);
        assert!(
            process
                .capture_bounded(tools.program("kache"), &["stats"], "bounded report", 1)
                .is_err()
        );
        let event = json!({"cache_key": "a".repeat(64), "unit_id": "b".repeat(16), "schema": 20,
            "key_fields": {"compiler": "c".repeat(16), "args": "AWS_SECRET_ACCESS_KEY=secret", "AWS_SECRET_ACCESS_KEY": "secret"},
            "key_env_deps": {"AWS_SECRET_ACCESS_KEY": "secret"}, "command": "secret"});
        let runtime = process.environment_path("KACHE_RUNTIME_DIR").unwrap();
        fs::write(
            runtime.join("events.jsonl"),
            format!("{}invalid\n", format!("{event}\n").repeat(129)),
        )
        .unwrap();
        let transfer = json!({"direction": "download", "outcome": "import_error", "ok": false,
            "cache_key": "d".repeat(64), "elapsed_ms": 5012, "request_ms": 2,
            "import_lock_wait_ms": 5000, "object_key": "AWS_SECRET_ACCESS_KEY=secret"});
        fs::write(runtime.join("transfers.jsonl"), format!("{transfer}\n")).unwrap();
        let retained = history(&process).unwrap();
        assert_eq!(retained["records"].as_array().unwrap().len(), 128);
        assert_eq!(retained["truncated"], true);
        assert_eq!(retained["invalid_records"], 1);
        assert!(retained["prior_jobs"].is_null());
        assert_eq!(
            retained["records"][0]["key_fields"]["compiler"],
            "c".repeat(16)
        );
        assert!(!retained.to_string().contains("secret"));
        retain(
            &process,
            &tools,
            &Ok("{}".into()),
            &Ok(String::new()),
            "test",
        )
        .unwrap();
        let path = report_path(&process, "test").unwrap();
        let report: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(report["key_history"], retained);
        assert_eq!(
            report["transfer_history"]["records"][0]["outcome"],
            "import_error"
        );
        assert_eq!(
            report["transfer_history"]["records"][0]["cache_key"],
            "d".repeat(64)
        );
        assert_eq!(
            report["transfer_history"]["records"][0]["import_lock_wait_ms"],
            5000
        );
        assert_eq!(report["job"]["gitlab_job_id"], 123);
        assert_eq!(report["revision"], "a".repeat(40));
        assert!(report["job"]["github_run_id"].is_null());
        assert!(!report.to_string().contains("secret"));
        job.stop(&process, &tools);
        drop(job);
        assert!(!runtime.exists());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            report
        );
        clear(&process, "test");
        assert!(!path.exists());
    }

    #[test]
    fn inherited_producer_reports_are_not_a_consumers_upload_target() {
        let root = tempfile::tempdir().unwrap();
        let producer = Process::new(
            root.path(),
            std::collections::BTreeMap::from([("CI_JOB_ID".into(), "1".into())]),
        );
        let consumer = Process::new(
            root.path(),
            std::collections::BTreeMap::from([("CI_JOB_ID".into(), "2".into())]),
        );
        let producer_path = report_path(&producer, "apple-lint").unwrap();
        fs::create_dir_all(producer_path.parent().unwrap()).unwrap();
        fs::write(&producer_path, "upstream report").unwrap();
        let consumer_path = report_path(&consumer, "apple-test").unwrap();
        assert_eq!(
            consumer_path.file_name().unwrap(),
            "gitlab-2-apple-test.json"
        );
        assert_ne!(producer_path, consumer_path);
        assert!(
            !consumer_path.exists(),
            "bootstrap failure must leave no current-job report to upload"
        );
        assert!(producer_path.exists());
    }
}
