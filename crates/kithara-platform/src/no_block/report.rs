use std::{env, fmt::Write as _, fs::OpenOptions, io::Write, panic::Location, time::Duration};

use super::{
    mode::{Mode, log_path, mode},
    watch::Tier,
};

struct Consts;

impl Consts {
    const SPIN_CPU_FRACTION: f64 = 0.8;
    const MAX_RUN_ID_BYTES: usize = 1_024;
    const MAX_ATTEMPT_ID_BYTES: usize = 1_024;
    const MAX_CANONICAL_FIELD_BYTES: usize = 512;
}

enum OverBudgetAction {
    Ignore,
    Census,
    Panic,
}

pub(super) fn forbidden(
    what: &'static str,
    task: &'static str,
    spawned: &'static Location<'static>,
    at: &'static Location<'static>,
) {
    match mode() {
        Mode::Off => {}
        Mode::Census => {
            let line = format!(
                "[no_block][census] blocking {what} inside async poll of `{task}` \
                 (spawned at {spawned}) at {at}"
            );
            census_emit(&line);
        }
        Mode::Panic => panic!(
            "[no_block] blocking {what} inside async poll of `{task}` \
             (spawned at {spawned})\n  at {at}\n  sanctioned bridge? mark the fn \
             with #[kithara::allow_block]"
        ),
    }
}

pub(super) fn bridged(
    task: &'static str,
    spawned: &'static Location<'static>,
    spawn_loc: Option<&'static Location<'static>>,
) {
    let flash_task = spawn_loc.map_or_else(|| "-".to_owned(), ToString::to_string);
    match mode() {
        Mode::Off => {}
        Mode::Census => {
            let line = format!(
                "[no_block][census] BRIDGED sync wait inside async poll of `{task}` \
                 (spawned at {spawned}, flash task {flash_task}) - in prod this blocks \
                 a runtime worker"
            );
            census_emit(&line);
        }
        Mode::Panic => panic!(
            "[no_block] BRIDGED sync wait inside async poll of `{task}` \
             (spawned at {spawned}, flash task {flash_task}) - in prod this blocks \
             a runtime worker"
        ),
    }
}

pub(super) fn over_budget(
    task: &'static str,
    spawned: &'static Location<'static>,
    wall: Duration,
    cpu: Option<Duration>,
    budget: Duration,
    tier: Tier,
) {
    let kind = classify(wall, cpu);
    let action = match mode() {
        Mode::Off => OverBudgetAction::Ignore,
        Mode::Census => OverBudgetAction::Census,
        Mode::Panic => match tier {
            Tier::Blanket => match kind {
                "CPU spin" => OverBudgetAction::Panic,
                _ => OverBudgetAction::Census,
            },
            Tier::Strict => OverBudgetAction::Panic,
        },
    };

    match action {
        OverBudgetAction::Ignore => {}
        OverBudgetAction::Census => {
            let line = format!(
                "[no_block][census] task `{task}` (spawned at {spawned}): single poll took \
                 {wall:?} (cpu {cpu:?}, budget {budget:?}) - {kind}"
            );
            census_emit(&line);
        }
        OverBudgetAction::Panic => panic!(
            "[no_block] task `{task}` (spawned at {spawned}): single poll took {wall:?} \
             (cpu {cpu:?}, budget {budget:?}) - {kind}\n  sanctioned blocking? mark \
             the blocking fn with #[kithara::allow_block]"
        ),
    }
}

fn census_emit(line: &str) {
    let line = format!("{}{line}", nextest_prefix());
    tracing::warn!(target: "kithara_platform::no_block", "{line}");

    if let Some(path) = log_path() {
        let line = format!("{line}\n");
        if let Err(error) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
        {
            panic!(
                "[no_block] failed to write census log `{}`: {error}",
                path.display()
            );
        }
    }
}

pub(super) fn nextest_prefix() -> String {
    let read = |key| env::var(key).ok().filter(|value| !value.is_empty());
    let run_id = read("NEXTEST_RUN_ID");
    let attempt_id = read("NEXTEST_ATTEMPT_ID");
    let binary_id = read("NEXTEST_BINARY_ID");
    let test_name = read("NEXTEST_TEST_NAME");
    let stress_current = read("NEXTEST_STRESS_CURRENT");
    nextest_prefix_from(
        run_id.as_deref(),
        attempt_id.as_deref(),
        binary_id.as_deref(),
        test_name.as_deref(),
        stress_current.as_deref(),
    )
}

fn nextest_prefix_from(
    run_id: Option<&str>,
    attempt_id: Option<&str>,
    binary_id: Option<&str>,
    test_name: Option<&str>,
    stress_current: Option<&str>,
) -> String {
    let mut fields = Vec::with_capacity(5);
    if let Some(run_id) = run_id.filter(|value| !value.is_empty()) {
        fields.push(format!(
            "run_id={}",
            encode_metadata(run_id, Consts::MAX_RUN_ID_BYTES)
        ));
    }
    if let Some(attempt_id) = attempt_id.filter(|value| !value.is_empty()) {
        fields.push(format!(
            "attempt_id={}",
            encode_metadata(attempt_id, Consts::MAX_ATTEMPT_ID_BYTES)
        ));
    }
    if let Some(binary_id) = binary_id.filter(|value| !value.is_empty()) {
        fields.push(format!(
            "binary_id={}",
            encode_metadata(binary_id, Consts::MAX_CANONICAL_FIELD_BYTES)
        ));
    }
    if let Some(test_name) = test_name.filter(|value| !value.is_empty()) {
        fields.push(format!(
            "test_name={}",
            encode_metadata(test_name, Consts::MAX_CANONICAL_FIELD_BYTES)
        ));
    }
    if let Some(stress_current) = stress_current.filter(|value| !value.is_empty()) {
        fields.push(format!(
            "stress_current={}",
            encode_metadata(stress_current, Consts::MAX_CANONICAL_FIELD_BYTES)
        ));
    }

    if fields.is_empty() {
        String::new()
    } else {
        format!("[nextest {}] ", fields.join(" "))
    }
}

fn encode_metadata(value: &str, max_bytes: usize) -> String {
    let mut encoded = String::with_capacity(value.len().min(max_bytes));
    for character in value.chars() {
        let piece = if character.is_ascii_alphanumeric()
            || matches!(character, '-' | '_' | '.' | ':' | '@' | '$' | '#' | '/')
        {
            character.to_string()
        } else {
            let mut utf8 = [0; 4];
            character.encode_utf8(&mut utf8).as_bytes().iter().fold(
                String::new(),
                |mut bytes, byte| {
                    let _ = write!(bytes, "%{byte:02X}");
                    bytes
                },
            )
        };
        if encoded.len().saturating_add(piece.len()) > max_bytes {
            if encoded.len() == max_bytes {
                encoded.pop();
            }
            encoded.push('~');
            break;
        }
        encoded.push_str(&piece);
    }
    encoded
}

fn classify(wall: Duration, cpu: Option<Duration>) -> &'static str {
    match cpu {
        Some(c) if c.as_secs_f64() >= wall.as_secs_f64() * Consts::SPIN_CPU_FRACTION => "CPU spin",
        Some(_) => "blocked wait (lock/sleep/IO)",
        None => "unclassified (no thread CPU clock)",
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const WALL: Duration = Duration::from_millis(50);

    #[kithara::test(native, flash(false))]
    fn classify_splits_cpu_spin_blocked_and_unclassified() {
        assert_eq!(classify(WALL, Some(Duration::from_millis(49))), "CPU spin");
        assert_eq!(
            classify(WALL, Some(Duration::from_millis(2))),
            "blocked wait (lock/sleep/IO)"
        );
        assert_eq!(classify(WALL, None), "unclassified (no thread CPU clock)");
    }

    #[kithara::test(native, flash(false))]
    fn nextest_prefix_includes_run_attempt_and_canonical_test_identity() {
        let prefix = nextest_prefix_from(
            Some("run-id"),
            Some("run-id:binary@stress-17$module::test_name#2"),
            Some("crate::integration"),
            Some("module::test_name"),
            Some("17"),
        );

        assert_eq!(
            prefix,
            "[nextest run_id=run-id \
             attempt_id=run-id:binary@stress-17$module::test_name#2 \
             binary_id=crate::integration test_name=module::test_name stress_current=17] "
        );
    }

    #[kithara::test(native, flash(false))]
    fn nextest_prefix_falls_back_to_test_and_stress_identity() {
        assert_eq!(
            nextest_prefix_from(
                Some("run-id"),
                None,
                Some("crate::integration"),
                Some("module::test_name"),
                Some("17")
            ),
            "[nextest run_id=run-id binary_id=crate::integration \
             test_name=module::test_name stress_current=17] "
        );
    }

    #[kithara::test(native, flash(false))]
    fn nextest_prefix_is_empty_without_nextest_metadata() {
        assert_eq!(nextest_prefix_from(None, None, None, None, None), "");
    }

    #[kithara::test(native, flash(false))]
    fn nextest_prefix_encodes_and_bounds_untrusted_metadata() {
        assert_eq!(
            nextest_prefix_from(Some("run\n:id]"), None, None, None, None),
            "[nextest run_id=run%0A:id%5D] "
        );

        let oversized_run = "x".repeat(Consts::MAX_RUN_ID_BYTES + 1);
        let prefix = nextest_prefix_from(Some(&oversized_run), None, None, None, None);
        let value = prefix
            .strip_prefix("[nextest run_id=")
            .and_then(|value| value.strip_suffix("] "))
            .expect("run prefix shape");
        assert_eq!(value.len(), Consts::MAX_RUN_ID_BYTES);
        assert!(value.ends_with('~'));

        let oversized = "x".repeat(Consts::MAX_ATTEMPT_ID_BYTES + 1);
        let prefix = nextest_prefix_from(None, Some(&oversized), None, None, None);
        let value = prefix
            .strip_prefix("[nextest attempt_id=")
            .and_then(|value| value.strip_suffix("] "))
            .expect("attempt prefix shape");
        assert_eq!(value.len(), Consts::MAX_ATTEMPT_ID_BYTES);
        assert!(value.ends_with('~'));
    }
}
