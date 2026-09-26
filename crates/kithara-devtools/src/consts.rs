use std::time::Duration;

#[cfg(feature = "lint")]
#[cfg(test)]
pub(crate) const GATE: &str = "#[cfg(all(not(target_arch = \"wasm32\"), feature = \"flash\"))]";

#[cfg(feature = "lint")]
#[cfg(test)]
pub(crate) const BASE_CRATES: &[&str] = &["kithara-platform", "kithara-abr", "kithara-drm"];

#[cfg(feature = "lint")]
#[cfg(test)]
pub(crate) const HIGH_CRATES: &[&str] = &[
    "kithara-hls",
    "kithara-file",
    "kithara-audio",
    "kithara-decode",
];

#[cfg(feature = "lint")]
#[cfg(test)]
pub(crate) const MID_CRATES: &[&str] = &[
    "kithara-storage",
    "kithara-bufpool",
    "kithara-assets",
    "kithara-net",
    "kithara-stream",
    "kithara-decode",
    "kithara-file",
    "kithara-hls",
    "kithara-audio",
    "kithara-events",
];

#[cfg(feature = "lint")]
#[cfg(test)]
pub(crate) const FACADE_CRATE: &str = "kithara";

#[cfg(test)]
pub(crate) const PASS_ENV: &str = "DEVTOOLS_AUDIT_CLIPPY_PASS";

#[cfg(test)]
pub(crate) const PASS_LOG: &str = "DEVTOOLS_AUDIT_CLIPPY_LOG";

#[cfg(test)]
pub(crate) const PASS_EXIT: &str = "DEVTOOLS_AUDIT_CLIPPY_EXIT";

pub(crate) const ASSESSMENT_DIRECTORY: &str = "quality-assessment";
pub(crate) const ASSESSMENT_MANIFEST: &str = "manifest.json";
pub(crate) const CRAP_DIRECTORY: &str = "cargo-crap";
pub(crate) const CRAP_REPORT: &str = "report.md";
pub(crate) const METRICS: &str = "metrics.json";
pub(crate) const SIMILARITY_ARTIFACT: &str = "similarity-report";
pub(crate) const SIMILARITY_REPORT: &str = "report.md";

/// Where the health report stops being a verdict and starts being logs.
pub(crate) const STAGE_DETAILS: &str = "## Stage details";

pub(crate) const BASELINE_SCHEMA_VERSION: u32 = 1;

pub(crate) const SECRET_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "CI_JOB_TOKEN",
    "CODECOV_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "NPM_TOKEN",
    "OPENAI_API_KEY",
];

/// Budget for the tests whose subject is not the budget.
///
/// [`terminates_timed_out_process`] owns the timeout contract and proves it
/// against a child that never finishes on its own. Everywhere else a
/// reachable budget can only decide the outcome by firing, and a killed
/// child reports no exit code — so a busy host would quietly substitute
/// "the machine was slow" for the exit-status mapping under test.
#[cfg(test)]
pub(crate) const NOT_UNDER_TEST: Duration = Duration::MAX;

#[cfg(test)]
pub(crate) const PROCESS_CHILD_ENV: &str = "DEVTOOLS_PROCESS_CHILD";

#[cfg(test)]
pub(crate) const CHILD_EMIT: &str = "emit";

#[cfg(test)]
pub(crate) const CHILD_SLEEP: &str = "sleep";

#[cfg(test)]
pub(crate) const PROCESS_CHILD_EXIT_CODE: i32 = 3;

#[cfg(test)]
pub(crate) const PROCESS_STDOUT_MARKER: &str = "devtools-process-stdout-marker";

#[cfg(test)]
pub(crate) const PROCESS_STDERR_MARKER: &str = "devtools-process-stderr-marker";

pub(crate) const PROJECT_CONFIG_REL: &str = ".config/xtask.toml";

pub(crate) const RULE_BAR: &str =
    "─────────────────────────────────────────────────────────────────────";

pub(crate) const GIT_LISTING_ARGS: [&str; 4] =
    ["ls-files", "--cached", "--others", "--exclude-standard"];

/// Substrings that mark an environment-level failure rather than a real
/// regression — typically a missing tool or unpublished baseline.
/// When any of these appear in the stage log on non-zero exit the stage
/// is reported as SKIP instead of FAIL.
pub(crate) const ENV_SKIP_MARKERS: &[&str] = &[
    "no such command:",
    "command not found",
    "not found in registry",
    "Library not loaded",
];

pub(crate) const BASELINE_CONFIG_DIRS: &[&str] =
    &[".config/arch", ".config/style", ".config/idioms"];

pub(crate) const COMMENTED_CONFIG_TEMPLATE: &str = r#"
# Optional generic tooling config sections. Uncomment only the settings this workspace owns.
#
# [health]
# feature_powerset_exclude = []
# machete_exclude = []
# lockbud_exclude = []
# semver_packages = []
# geiger_package = ""
#
# [test]
# default_lane = ""
# default_backend = ""
# feature_arg = ""
# features = []
#
# [test.flash]
# features = []
# default = true
#
# [test.lanes.default]
# program = ""
# prefix_args = []
# suffix_args = []
# default_features = []
# default_flash = true
# default_no_block = false
# passthrough = ""
#
# [test.net_backends.default]
# features = []
#
# [perf]
# primary_lane = ""
# nextest_profile = "perf"
# frame_prefix = ""
#
# [[perf.lanes]]
# flash = true
# backend = ""
#
# [orphans]
# exclude_packages = []
#
# [quality]
# unimock_traits_dir = ""
#
# [lint_exclude]
# paths = []
# modules = []
# scan_all_rules = []
#
# [workspace-scan]
# exclude = []
"#;

pub(crate) const MAIN_RS_SNIPPET: &str = r#"use clap::{Parser, Subcommand};
use kithara_devtools::{CoreCommand, Ctx};

#[derive(Debug, Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    Core(CoreCommand),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx::load()?;
    match cli.command {
        Command::Core(cmd) => kithara_devtools::run(&cmd, &ctx),
    }
}
"#;

pub(crate) const MAX_JUNIT_CASES: usize = 750_000;
pub(crate) const MAX_CASE_OUTPUT_BYTES: usize = 8 * 1_024 * 1_024;

#[cfg(test)]
pub(crate) const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2" failures="1">
  <testsuite name="demo-tests::suite_light" tests="2" failures="1">
    <testcase name="offline::gapless" classname="demo-tests::suite_light" time="1.532"/>
    <testcase name="offline::seek" classname="demo-tests::suite_light" time="0.201">
      <failure type="test failure">boom</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

/// What nextest writes for a test that failed an attempt and passed a later
/// one: the failing attempt is described inside `flakyFailure`, streams
/// included, while the `testcase` keeps the streams of the attempt that
/// passed.
#[cfg(test)]
pub(crate) const RETRIED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1" failures="0">
  <testsuite name="demo-tests::suite_stress" tests="1" failures="0">
    <testcase name="abr::switch" classname="demo-tests::suite_stress" time="2.100">
      <flakyFailure type="test failure" message="boom">panicked at abr.rs:7
        <system-out>red stdout</system-out>
        <system-err>red stderr</system-err>
      </flakyFailure>
      <system-out>green stdout</system-out>
      <system-err></system-err>
    </testcase>
  </testsuite>
</testsuites>"#;

/// What nextest writes for a failed `assert_eq!`: the panic header is
/// lifted into `message` and the body repeats it verbatim.
#[cfg(test)]
pub(crate) const PANIC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1" failures="1">
  <testsuite name="demo-tests::suite_light" tests="1" failures="1">
    <testcase name="audio::warms_pool" classname="demo-tests::suite_light" time="0.536">
      <failure message="thread 'audio::warms_pool' (971370) panicked at tests/demo.rs:166:5" type="test failure with exit code 101">thread 'audio::warms_pool' (971370) panicked at tests/demo.rs:166:5:
assertion `left == right` failed: a warmed pool must serve decode-sized buffers without allocating
  left: 0
 right: 1
stack backtrace:
   0: __rustc::rust_begin_unwind</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

pub(crate) const SWEEP_INSTALL_HINT: &str = "cargo install cargo-modules";
pub(crate) const MARKER: &str = "orphaned module `";

/// A cgroup v2 job container reports its own cap here; a machine without
/// one has no such file and is bounded by its cores alone.
pub(crate) const MEMORY_MAX: &str = "/sys/fs/cgroup/memory.max";

/// What one `cargo modules` run needs. It loads the whole workspace into a
/// rust-analyzer database and peaked at three gibibytes on this one, so
/// the budget is that measurement with room for the workspace to grow.
pub(crate) const WORKER_MEMORY: u64 = 3584 * 1024 * 1024;

#[cfg(test)]
pub(crate) const GIB: u64 = 1024 * 1024 * 1024;

pub(crate) const DEPTH: &str = "2";
pub(crate) const MOCK_COVERAGE_PATTERN: &str = r"(unimock::unimock\(|#\[\s*kithara::mock)";
pub(crate) const COLLECT_SCHEMA_VERSION: u32 = 2;

pub(crate) const CHA_PLUGINS: &str = "data_clumps,feature_envy,inappropriate_intimacy,shotgun_surgery,\
                           divergent_change,speculative_generality,async_callback_leak";

pub(crate) const CRAP_THRESHOLD: f64 = 30.0;
pub(crate) const QUALITY_LAB_CONFIG_REL: &str = ".config/quality-lab.toml";

#[cfg(test)]
pub(crate) const VALID_CONFIG: &str = r#"
schema_version = 1
output_dir = "target/quality-lab"

[profiles.coverage]
timeout_secs = 1800
tools = ["cargo-crap"]

[profiles.scheduled]
timeout_secs = 900
tools = ["cha", "rustqual", "cargo-dupes"]

[profiles.manual]
timeout_secs = 1800
tools = ["cha", "rustqual", "cargo-dupes", "pmat"]

[tools.cargo-crap]
version = "0.3.1"
timeout_secs = 300

[tools.cha]
version = "1.20.0"
timeout_secs = 300

[tools.rustqual]
version = "1.8.1"
timeout_secs = 300

[tools.cargo-dupes]
version = "0.2.1"
timeout_secs = 300

[tools.pmat]
version = "3.24.2"
timeout_secs = 1800
"#;

pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The name of the double's binary target.
#[cfg(test)]
pub(crate) const DOUBLE: &str = "kithara-devtools-fake-tool";

/// The version the double answers `--version` with.
///
/// It reaches the double through its behaviour file rather than being
/// compiled into it, so this test and the double cannot drift apart.
#[cfg(test)]
pub(crate) const DOUBLE_VERSION: &str = "1.2.3";

/// The profile nextest runs when a command names none, and the profile every
/// other one inherits its unset keys from.
pub(crate) const DEFAULT_PROFILE: &str = "default";

#[cfg(test)]
pub(crate) const RETRYING_PROFILE: &str = "\
[profile.default]
fail-fast = false

[profile.ci]
retries = 1

[profile.ci.junit]
path = \"junit.xml\"
";

/// The shape nextest writes for a case it retried into a pass: the failing
/// attempt lives inside `flakyFailure`, and the run reports no failures.
#[cfg(test)]
pub(crate) const RETRIED_PASS: &str = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<testsuites name=\"nextest-run\" tests=\"1\" failures=\"0\" errors=\"0\" uuid=\"1\" timestamp=\"t\" time=\"0.049\">
    <testsuite name=\"kithara_queue\" tests=\"1\" disabled=\"0\" errors=\"0\" failures=\"0\">
        <testcase name=\"delayed_target\" classname=\"kithara_queue\" time=\"0.019\">
            <flakyFailure message=\"panicked at delayed.rs:9\" type=\"test failure with exit code 101\">assertion failed</flakyFailure>
        </testcase>
    </testsuite>
</testsuites>
";

/// Environment variable naming the compiler-cache wrapper Cargo runs.
pub(crate) const WRAPPER: &str = "RUSTC_WRAPPER";

/// Environment variable Cargo reads to decide on incremental compilation.
pub(crate) const INCREMENTAL: &str = "CARGO_INCREMENTAL";

pub(crate) const SEMVER_INSTALL_HINT: &str = "cargo install cargo-semver-checks";

#[cfg(test)]
pub(crate) const LOCK: &str = r#"
version = 4

[[package]]
name = "anyhow"
version = "1.0.100"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "kithara-decode"
version = "0.0.1-alpha4"
dependencies = ["anyhow"]

[[package]]
name = "firewheel-web-audio"
version = "0.7.0"
source = "git+https://github.com/example/firewheel#0000000"
"#;

pub(crate) const SIMILARITY_CONFIG_REL: &str = ".config/similarity.toml";
pub(crate) const CONFIG_INSTALL_HINT: &str = "cargo install similarity-rs";

/// Hands the run's repeat count to a lane that performs its own repeats.
pub(crate) const REPEATS_ENV: &str = "KITHARA_STRESS_REPEATS";

#[cfg(test)]
pub(crate) const VIOLATION: &str = "\
==2534==ERROR: RealtimeSanitizer: unsafe-library-call
Intercepted call to real-time unsafe function `malloc` in real-time context!
    #0 0x5628d3a1b2c0 in malloc (/opt/bin/suite_stress+0x1042c0)
    #1 0x5628d3c11f30 in kithara_audio::renderer::mix crates/kithara-audio/src/renderer/mix.rs:214:23
";

/// Where a launched lane records what repeat count it was handed.
#[cfg(test)]
pub(crate) const REPEATS_RECORD_ENV: &str = "DEVTOOLS_STRESS_REPEATS_RECORD";

#[cfg(test)]
pub(crate) const SUITE: &str = "kithara-integration-tests::rtsan";

#[cfg(test)]
pub(crate) const CASE: &str = "audio::mix_tap";

/// The environment variable every `cargo` invocation reads to decide where it
/// builds, and the one a stress run cannot afford to inherit.
pub(crate) const TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";

pub(crate) const MANIFEST_READ_LIMIT: u64 = 1_048_577;
pub(crate) const MANIFEST_SCHEMA: u32 = 4;
pub(crate) const MAX_MANIFEST_BYTES: usize = 1_048_576;

#[cfg(test)]
pub(crate) const CONTROLLER_SHA: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[cfg(test)]
pub(crate) const SUBJECT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// Caps queued output at 128 `KiB`; full queues apply backpressure to readers.
pub(crate) const CHANNEL_DEPTH: usize = 8;

/// Bounds reader-failure detection latency while the sibling stream is idle.
pub(crate) const READER_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Amortizes reads without accumulating a complete stress log in memory.
pub(crate) const READ_BUFFER_BYTES: usize = 16 * 1024;

#[cfg(test)]
pub(crate) const OUTPUT_CHILD_ENV: &str = "DEVTOOLS_STRESS_OUTPUT_CHILD";

#[cfg(test)]
pub(crate) const CHILD_COMPLETION_FILE: &str = "DEVTOOLS_STRESS_OUTPUT_COMPLETION_FILE";

#[cfg(test)]
pub(crate) const CHILD_ENV_VALUE: &str = "emit";

#[cfg(test)]
pub(crate) const BLOCK_CHILD_ENV_VALUE: &str = "emit-then-block";

#[cfg(test)]
pub(crate) const OUTPUT_CHILD_EXIT_CODE: i32 = 23;

#[cfg(test)]
pub(crate) const OUTPUT_STDERR_MARKER: &[u8] = b"stress-output-stderr-marker\n";

#[cfg(test)]
pub(crate) const OUTPUT_STDOUT_MARKER: &[u8] = b"stress-output-stdout-marker\n";

pub(crate) const SCHEMA: &str = "devtools.pressure.v2";

pub(crate) const CGROUP_METRICS: &[(&str, &str)] = &[
    ("cgroup.cpu.stat", "cpu.stat"),
    ("cgroup.cpu.pressure", "cpu.pressure"),
    ("cgroup.memory.current", "memory.current"),
    ("cgroup.memory.peak", "memory.peak"),
    ("cgroup.memory.events", "memory.events"),
    ("cgroup.memory.pressure", "memory.pressure"),
    ("cgroup.io.stat", "io.stat"),
    ("cgroup.io.pressure", "io.pressure"),
    ("cgroup.pids.current", "pids.current"),
    ("cgroup.pids.events", "pids.events"),
    ("cgroup.cpuset.cpus.effective", "cpuset.cpus.effective"),
];

pub(crate) const MAX_METRIC_BYTES: usize = 32 * 1_024;
pub(crate) const MAX_METRIC_READ_BYTES: u64 = 32 * 1_024 + 1;
pub(crate) const MAX_RECORD_BYTES: usize = 1_048_576;
pub(crate) const MAX_RECOVERY_BYTES: u64 = 2 * 1_048_576 + 2;

pub(crate) const MEMINFO_FIELDS: &[&str] = &[
    "MemTotal",
    "MemFree",
    "MemAvailable",
    "Buffers",
    "Cached",
    "SwapTotal",
    "SwapFree",
];

pub(crate) const PROC_METRICS: &[(&str, &str)] = &[
    ("proc.pressure.cpu", "pressure/cpu"),
    ("proc.pressure.memory", "pressure/memory"),
    ("proc.pressure.io", "pressure/io"),
];

pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const CGROUP_ROOT: &str = "/sys/fs/cgroup";
pub(crate) const PROC_ROOT: &str = "/proc";

/// The first line the flash engine writes into a hang dump.
#[cfg(test)]
pub(crate) const ENGINE_COUNTERS: &str =
    "virtual_now_ns=86410020000000 active=1 active_async=0 real_io=0 pace_anchor=none yielders=0";

#[cfg(test)]
pub(crate) const STEADY: &str =
    "2026-08-16T01:23:45.700000Z INFO kithara_play::session: playback completed frames=220500";

pub(crate) const MAX_ENVELOPE_BYTES: u64 = 4 * 1_024 * 1_024;
pub(crate) const MAX_ENVELOPE_DIRECTORY_ENTRIES: usize = 100_000;
pub(crate) const EVIDENCE_LINE_BYTES: usize = 64 * 1_024;
pub(crate) const PRESSURE_LINE_BYTES: usize = 1_048_576;

/// Written to the lane log before each attempt so findings can be attributed.
pub(crate) const ATTEMPT_MARKER: &str = "[kithara_stress] attempt ";

#[cfg(test)]
pub(crate) const UNSAFE_CALL: &str = "\
==2534==ERROR: RealtimeSanitizer: unsafe-library-call
Intercepted call to real-time unsafe function `malloc` in real-time context!
    #0 0x5628d3a1b2c0 in malloc (/opt/bin/suite_light+0x1042c0)
    #1 0x5628d3b0e1a4 in alloc::alloc::alloc /rustc/abc/library/alloc/src/alloc.rs:100:9
    #2 0x5628d3c11f30 in kithara_audio::renderer::mix crates/kithara-audio/src/renderer/mix.rs:214:23

test kithara_play::rt_metrics::a_healthy_track_reports_no_trouble ... ok
";

#[cfg(test)]
pub(crate) const BLOCKING_CALL: &str = "\
==2534==ERROR: RealtimeSanitizer: blocking-call
Call to blocking function `pthread_mutex_lock` in real-time context!
    #0 0x5628d3a1b2c0 in pthread_mutex_lock (/opt/bin/suite_light+0x1042c0)
    #1 0x5628d3c11f30 in kithara_audio::renderer::slot crates/kithara-audio/src/renderer/slot.rs:88:9
";

pub(crate) const PERCENT_SCALE: usize = 100;
pub(crate) const MAX_INVENTORY_CASES: usize = 100_000;
pub(crate) const MAX_INVENTORY_BYTES: u64 = 64 * 1_024 * 1_024;
pub(crate) const MAX_JUNIT_BYTES: u64 = 512 * 1_024 * 1_024;

/// Bounds a lane log, which a run appends to once per attempt.
pub(crate) const MAX_LANE_LOG_BYTES: u64 = 512 * 1_024 * 1_024;

#[cfg(test)]
pub(crate) const PASSED_JUNIT: &str = r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z">
  <testsuite name="demo::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:00Z"/>
  </testsuite>
</testsuites>"#;

pub(crate) const FLASH_TOGGLE: &str = "flash";
pub(crate) const NO_BLOCK_TOGGLE: &str = "no-block";
pub(crate) const CONFIG_PATH: &str = ".config/typos.toml";
pub(crate) const TYPOS_INSTALL_HINT: &str = "cargo install typos-cli";
