use std::ops::Range;

use anyhow::{Context, Result, bail};

use super::profile::{LinuxHost, LinuxRunner};

/// CPU sets in profile order, reserving stress capacity before ordinary jobs.
///
/// # Errors
///
/// Rejects reservations that cannot preserve every runner's CPU count.
pub(super) fn assignments(host: &LinuxHost, cores: usize) -> Result<Vec<String>> {
    if cores == 0 {
        bail!("Linux CI CPU allocation needs at least one CPU");
    }
    let requests = host
        .runners
        .iter()
        .map(|runner| {
            usize::try_from(runner.cpus)
                .context("Linux CI CPU request does not fit usize")
                .map(|cpus| cpus.clamp(1, cores))
        })
        .collect::<Result<Vec<_>>>()?;
    let reserved = host
        .runners
        .iter()
        .zip(&requests)
        .filter(|(runner, _)| is_stress(runner))
        .try_fold(0usize, |total, (_, cpus)| {
            total
                .checked_add(*cpus)
                .context("Linux CI stress CPU reservation overflow")
        })?;
    if reserved > cores {
        bail!("Linux CI stress runners need {reserved} CPUs, but the host has {cores}");
    }
    let ordinary = cores - reserved;
    if host
        .runners
        .iter()
        .zip(&requests)
        .any(|(runner, cpus)| !is_stress(runner) && *cpus > ordinary)
    {
        bail!(
            "Linux CI ordinary CPU requests do not fit the {ordinary} CPUs left after reserving {reserved} stress CPUs"
        );
    }
    let mut stress_offset = 0;
    let mut ordinary_index = 0;
    Ok(host
        .runners
        .iter()
        .zip(requests)
        .map(|(runner, cpus)| {
            let (index, pool) = if is_stress(runner) {
                let first = stress_offset;
                stress_offset += cpus;
                (0, first..stress_offset)
            } else {
                let index = ordinary_index;
                ordinary_index += 1;
                (index, reserved..cores)
            };
            cpuset(index, cpus, pool)
        })
        .collect())
}

fn is_stress(runner: &LinuxRunner) -> bool {
    runner
        .labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case("kithara-stress"))
}

/// Ordinary sets wrap within their pool; their size remains the job's nproc.
fn cpuset(index: usize, cpus: usize, pool: Range<usize>) -> String {
    let cpus = cpus.clamp(1, pool.len());
    let first = index.saturating_mul(cpus) % pool.len();
    pool.cycle()
        .skip(first)
        .take(cpus)
        .map(|cpu| cpu.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::ci::host::linux::profile::tests::host_fixture;

    fn stress(host: &LinuxHost, name: &str, cpus: u32) -> LinuxRunner {
        let mut runner = host.runners[0].clone();
        runner.name = name.to_owned();
        runner.cpus = cpus;
        runner.labels = vec!["kithara-stress".to_owned()];
        runner
    }

    fn set(value: &str) -> BTreeSet<usize> {
        value
            .split(',')
            .map(|cpu| cpu.parse().expect("CPU index"))
            .collect()
    }

    #[test]
    fn stress_reservations_ignore_profile_position_and_do_not_overlap() {
        let mut host = host_fixture();
        for runner in &mut host.runners {
            runner.cpus = 3;
        }
        let first = stress(&host, "stress-a", 8);
        let mut second = stress(&host, "stress-b", 4);
        second.labels = vec!["KITHARA-STRESS".to_owned()];
        host.runners.insert(1, first);
        host.runners.push(second);
        let actual = assignments(&host, 32).unwrap();
        let a = set(&actual[1]);
        let b = set(actual.last().unwrap());
        assert_eq!(a.len(), 8);
        assert_eq!(b.len(), 4);
        assert!(a.is_disjoint(&b));
        for (runner, cpus) in host.runners.iter().zip(&actual) {
            assert_eq!(set(cpus).len(), runner.cpus as usize);
            assert!(set(cpus).iter().all(|cpu| *cpu < 32));
            if !is_stress(runner) {
                assert!(set(cpus).is_disjoint(&a));
                assert!(set(cpus).is_disjoint(&b));
            }
        }
    }

    #[test]
    fn an_impossible_reservation_does_not_shrink_an_ordinary_job() {
        let mut host = host_fixture();
        host.runners.push(stress(&host, "stress", 9));
        let error = assignments(&host, 32).expect_err("ordinary runner needs 24 CPUs");
        assert!(error.to_string().contains("ordinary CPU requests"));
    }

    #[test]
    fn stress_runners_cannot_oversubscribe_their_reserved_pool() {
        let mut host = host_fixture();
        host.runners = vec![stress(&host, "first", 24), stress(&host, "second", 24)];
        assert!(assignments(&host, 32).is_err());
        host.runners.pop();
        host.runners[0].cpus = 64;
        assert_eq!(set(&assignments(&host, 32).unwrap()[0]).len(), 32);
        assert!(assignments(&host, 0).is_err());
    }

    #[test]
    fn a_runner_is_given_exactly_the_cores_it_asked_for() {
        for index in 0..12 {
            let set = cpuset(index, 3, 0..32);
            let cores = set.split(',').collect::<Vec<_>>();
            assert_eq!(cores.len(), 3, "index {index}: {set}");
            assert_eq!(
                cores.iter().collect::<BTreeSet<_>>().len(),
                3,
                "index {index} repeats a core: {set}"
            );
        }
    }

    #[test]
    fn the_sets_wrap_instead_of_running_out() {
        assert_eq!(cpuset(11, 3, 0..32), "1,2,3");
        assert_eq!(cpuset(0, 4, 0..4), "0,1,2,3");
        assert_eq!(cpuset(0, 9, 0..4), "0,1,2,3");
    }
}
