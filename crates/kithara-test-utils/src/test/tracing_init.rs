use tracing_subscriber::EnvFilter;

pub fn setup_tracing() {
    setup_tracing_with_filter("warn");
}

/// Under the sanitizer the filter is never built. `init_tracing` installs no
/// subscriber there, but constructing an `EnvFilter` parses the directives and
/// allocates, and the test body it is called from sits inside the checked
/// region — the sanitizer reported it as an unsafe `malloc` in a real-time
/// context, attributed to whichever test happened to run first.
#[cfg(rtsan)]
pub fn setup_tracing_with_filter(_directives: &str) {}

#[cfg(not(rtsan))]
pub fn setup_tracing_with_filter(directives: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    crate::hang::install_panic_dump();
    let env = std::env::var(EnvFilter::DEFAULT_ENV).ok();
    init_tracing(EnvFilter::new(merged_directives(
        env.as_deref(),
        directives,
    )));
}

/// A run-wide `RUST_LOG` and a test's own `tracing(...)` directives are
/// both wanted: a lane widens the evidence for every test, and a test keeps the
/// targets the lane never names. Choosing one source over the other dropped the
/// other entirely, so a lane that set `RUST_LOG` to widen the evidence silenced
/// the annotated tests it was widened for.
///
/// A target named by both keeps the more verbose of the two levels, and appears
/// once: neither source can silence the other, and the result never depends on
/// how `EnvFilter` resolves a target repeated in one string.
///
/// Only `level` and `target=level` are merged. Span and field syntax
/// (`target[span{field}]=level`) is passed through untouched — this reads
/// directives, it does not reimplement them.
#[cfg(not(rtsan))]
fn merged_directives(env: Option<&str>, directives: &str) -> String {
    let Some(env) = env.map(str::trim).filter(|env| !env.is_empty()) else {
        return directives.to_owned();
    };
    let mut levels: Vec<(Option<&str>, usize)> = Vec::new();
    let mut passed_through: Vec<&str> = Vec::new();
    for part in directives
        .split(',')
        .chain(env.split(','))
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match read_directive(part) {
            Some((target, level)) => match levels.iter_mut().find(|(seen, _)| *seen == target) {
                Some(entry) => entry.1 = entry.1.max(level),
                None => levels.push((target, level)),
            },
            None => passed_through.push(part),
        }
    }
    let merged = levels
        .into_iter()
        .map(|(target, level)| {
            let level = LEVELS[level];
            target.map_or_else(|| level.to_owned(), |target| format!("{target}={level}"))
        })
        .chain(passed_through.into_iter().map(str::to_owned));
    merged.collect::<Vec<_>>().join(",")
}

/// Verbosity order, so a merge can take the wider of two levels.
#[cfg(not(rtsan))]
const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

/// `(target, verbosity)` of a plain directive, or `None` for anything carrying
/// span or field syntax. A bare target means every level, as in `RUST_LOG`.
#[cfg(not(rtsan))]
fn read_directive(part: &str) -> Option<(Option<&str>, usize)> {
    if part.contains('[') || part.contains(']') || part.contains('{') {
        return None;
    }
    let level_of = |name: &str| {
        LEVELS
            .iter()
            .position(|level| name.eq_ignore_ascii_case(level))
    };
    match part.split_once('=') {
        Some((target, level)) if !target.is_empty() => Some((Some(target), level_of(level)?)),
        Some(_) => None,
        None => Some(level_of(part).map_or((Some(part), LEVELS.len() - 1), |level| (None, level))),
    }
}

pub fn init_tracing(filter: EnvFilter) {
    // RealtimeSanitizer lane (`--cfg rtsan`, the native sanitizer build only):
    // a capturing/formatting subscriber allocates on the audio worker thread —
    // the forbid-blocking produce core — whenever it captures, or merely
    // *constructs* (recording `?`/`%` Debug fields keeps the callsite live via
    // the probe layer), a decode/seek diagnostic event. Production RT threads
    // are never synchronously logged, so install no subscriber here: callsites
    // stay disabled and the lane verifies kithara's own RT-safety, not the test
    // logger. Normal `just test` keeps the formatting subscriber.
    #[cfg(rtsan)]
    {
        let _ = filter;
    }

    #[cfg(all(not(target_arch = "wasm32"), not(rtsan)))]
    {
        use tracing_subscriber::{Layer as _, layer::SubscriberExt, util::SubscriberInitExt};

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);
        let _ = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(crate::flight::layer())
            .with(crate::test::usdt::layer())
            .try_init();
    }

    #[cfg(all(target_arch = "wasm32", not(rtsan)))]
    {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        use tracing_wasm::{WASMLayer, WASMLayerConfigBuilder};

        let mut config = WASMLayerConfigBuilder::new();
        config.set_report_logs_in_timings(false);
        let subscriber = tracing_subscriber::registry().with(filter);
        let subscriber = subscriber.with(WASMLayer::new(config.build()));
        let _ = subscriber.try_init();
    }
}

#[cfg(all(test, not(rtsan)))]
mod tests {
    use super::merged_directives;

    #[test]
    fn a_target_the_environment_never_names_survives() {
        let merged = merged_directives(Some("warn,kithara_hls=debug"), "kithara_abr=debug");

        assert!(merged.contains("kithara_abr=debug"), "merged: {merged}");
    }

    #[test]
    fn a_target_the_test_never_names_arrives() {
        let merged = merged_directives(Some("warn,kithara_hls=debug"), "kithara_abr=debug");

        assert!(merged.contains("kithara_hls=debug"), "merged: {merged}");
    }

    #[test]
    fn a_widened_environment_level_wins_the_shared_target() {
        let merged = merged_directives(Some("kithara_hls=trace"), "kithara_hls=info");

        assert_eq!(merged, "kithara_hls=trace");
    }

    #[test]
    fn a_narrowed_environment_level_does_not_silence_the_test() {
        let merged = merged_directives(Some("kithara_hls=info"), "kithara_hls=trace");

        assert_eq!(merged, "kithara_hls=trace");
    }

    #[test]
    fn a_shared_target_is_never_repeated() {
        let merged = merged_directives(Some("kithara_hls=trace"), "kithara_hls=info");

        assert_eq!(merged.matches("kithara_hls").count(), 1, "merged: {merged}");
    }

    #[test]
    fn the_global_level_merges_like_any_other() {
        let merged = merged_directives(Some("debug"), "warn");

        assert_eq!(merged, "debug");
    }

    #[test]
    fn a_bare_target_means_every_level() {
        let merged = merged_directives(Some("kithara_hls"), "kithara_hls=info");

        assert_eq!(merged, "kithara_hls=trace");
    }

    #[test]
    fn span_syntax_is_passed_through_untouched() {
        let merged = merged_directives(Some("[fetch{url}]=debug"), "warn");

        assert!(merged.contains("[fetch{url}]=debug"), "merged: {merged}");
    }

    #[test]
    fn an_unknown_level_is_passed_through_untouched() {
        let merged = merged_directives(Some("kithara_hls=verbose"), "warn");

        assert!(merged.contains("kithara_hls=verbose"), "merged: {merged}");
    }

    #[test]
    fn without_an_environment_the_test_directives_stand_alone() {
        let merged = merged_directives(None, "warn,kithara_hls=debug");

        assert_eq!(merged, "warn,kithara_hls=debug");
    }

    #[test]
    fn an_empty_environment_is_not_a_directive() {
        let merged = merged_directives(Some("   "), "warn,kithara_hls=debug");

        assert_eq!(merged, "warn,kithara_hls=debug");
    }
}
