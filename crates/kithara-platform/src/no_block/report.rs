use std::{fs::OpenOptions, io::Write, panic::Location, time::Duration};

use super::{
    mode::{Mode, log_path, mode},
    watch::Tier,
};

const SPIN_CPU_FRACTION: f64 = 0.8;

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
    eprintln!("{line}");

    if let Some(path) = log_path() {
        let line = format!("{line}\n");
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
    }
}

fn classify(wall: Duration, cpu: Option<Duration>) -> &'static str {
    match cpu {
        Some(c) if c.as_secs_f64() >= wall.as_secs_f64() * SPIN_CPU_FRACTION => "CPU spin",
        Some(_) => "blocked wait (lock/sleep/IO)",
        None => "unclassified (no thread CPU clock)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALL: Duration = Duration::from_millis(50);

    #[test]
    fn classify_splits_cpu_spin_blocked_and_unclassified() {
        assert_eq!(classify(WALL, Some(Duration::from_millis(49))), "CPU spin");
        assert_eq!(
            classify(WALL, Some(Duration::from_millis(2))),
            "blocked wait (lock/sleep/IO)"
        );
        assert_eq!(classify(WALL, None), "unclassified (no thread CPU clock)");
    }
}
