use std::{env, ffi::OsStr, process::Command};

/// Environment variable naming the compiler-cache wrapper Cargo runs.
const WRAPPER: &str = "RUSTC_WRAPPER";

/// Environment variable Cargo reads to decide on incremental compilation.
const INCREMENTAL: &str = "CARGO_INCREMENTAL";

/// Whether this run is one whose build cost is being accounted for.
fn in_ci() -> bool {
    set(env::var_os("CI").as_deref())
}

fn set(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Print the compiler cache's hit rate for the build that just ran.
///
/// A job that spends nine of its thirteen minutes building says nothing about
/// why until this number exists: a cache that is installed, enabled and missing
/// everything looks exactly like a cache that is working. The GitLab lane
/// executor has printed it for as long as it has existed; a job that reaches
/// `just` directly - which is every GitHub job - printed nothing.
///
/// Best effort by construction. This is a measurement appended to a lane, and a
/// lane's verdict is about the workspace, never about whether a cache daemon
/// answered.
pub(crate) fn report_stats() {
    if !worth_reporting(in_ci(), env::var_os(WRAPPER).as_deref()) {
        return;
    }
    match Command::new("sccache").arg("--show-stats").status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("sccache statistics were unavailable: {status}"),
        Err(error) => eprintln!("sccache statistics could not be collected: {error}"),
    }
}

/// A workstation runs the same recipes and wants its output to be the test
/// results, and a run with no wrapper has no cache to report on.
fn worth_reporting(in_ci: bool, wrapper: Option<&OsStr>) -> bool {
    in_ci && set(wrapper)
}

/// What a Clippy run must drop from its environment to get the caching that
/// suits where it runs, or nothing when what it inherited already suits it.
pub(crate) fn clippy_cleared() -> &'static [&'static str] {
    clippy_cleared_for(in_ci())
}

/// `sccache` aborts outright rather than fall back when it meets an incremental
/// build, so a Clippy run gets one of the two and never both. Which one is worth
/// more depends entirely on where it runs.
///
/// On a workstation, incremental: the dependencies are already built in the
/// local target directory, so the cache would serve almost nothing, while
/// incremental turns a fifteen-second re-check into two. Dropping the two
/// variables is what selects it - Cargo already compiles workspace crates
/// incrementally and registry ones never, so only the blanket
/// `CARGO_INCREMENTAL=0` the `justfile` exports was suppressing it. Answering
/// with `1` instead says the same thing to Cargo and one thing more to
/// everything else: `sccache` reads that variable too, so a C or C++ dependency
/// reaching it through a `CMake` compiler launcher dies on a Rust flag it never
/// used.
///
/// In CI, the cache: `sccache` is one content-addressed volume shared by every
/// runner, so one runner's entry is another's hit, while incremental state is
/// per-runner, invalidated by any toolchain or feature change, and was how a
/// build directory grew past two hundred gigabytes. Clearing the wrapper there
/// is what made every job re-check five hundred dependency crates from source.
///
/// Only `clippy-driver`'s own compilations - the workspace crates - go
/// uncached either way, which is the part this cannot help.
const fn clippy_cleared_for(in_ci: bool) -> &'static [&'static str] {
    if in_ci { &[] } else { &[WRAPPER, INCREMENTAL] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> Option<&OsStr> {
        Some(OsStr::new(text))
    }

    #[test]
    fn a_workstation_is_left_to_its_test_output() {
        assert!(!worth_reporting(false, value("sccache")));
    }

    #[test]
    fn a_job_without_a_wrapper_has_no_cache_to_report_on() {
        assert!(!worth_reporting(true, None));
        assert!(!worth_reporting(true, value("")));
    }

    #[test]
    fn a_cached_ci_build_is_accounted_for() {
        assert!(worth_reporting(true, value("sccache")));
    }

    #[test]
    fn a_ci_clippy_run_keeps_the_shared_cache() {
        assert!(clippy_cleared_for(true).is_empty());
    }

    #[test]
    fn a_workstation_clippy_run_trades_the_cache_for_incremental() {
        assert_eq!(clippy_cleared_for(false), [WRAPPER, INCREMENTAL]);
    }
}
