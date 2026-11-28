//! A verification that runs part of a merge's checks: what a passed draft of
//! the same merge left, or the jobs a rejected run of the same head failed.
//! The bridge starts it with a pipeline variable; the lanes and the verdict
//! read what it asks of them here.

use std::{collections::BTreeSet, env};

use anyhow::{Result, bail};

use crate::consts;

/// Whether this run follows its draft's passed run of the same merge.
pub(crate) fn follows_draft() -> bool {
    env::var(consts::QUARANTINE_REUSED_ENV).is_ok_and(|value| value == "1")
}

/// The jobs this run repeats, when it repeats only some.
pub(crate) fn repeated_jobs() -> Result<Option<BTreeSet<String>>> {
    env::var(consts::QUARANTINE_JOBS_ENV)
        .ok()
        .map(|pattern| jobs_of(&pattern))
        .transpose()
}

/// The regular expression a job's rules match its own name against: exactly
/// one of `jobs`.
pub(crate) fn pattern(jobs: &[String]) -> String {
    let names: Vec<String> = jobs
        .iter()
        .map(|job| {
            job.chars()
                .flat_map(|char| {
                    let plain = char.is_ascii_alphanumeric() || matches!(char, ':' | '-' | '_');
                    (!plain).then_some('\\').into_iter().chain([char])
                })
                .collect()
        })
        .collect();
    format!("/^({})$/", names.join("|"))
}

fn jobs_of(pattern: &str) -> Result<BTreeSet<String>> {
    let Some(names) = pattern
        .strip_prefix("/^(")
        .and_then(|rest| rest.strip_suffix(")$/"))
    else {
        bail!(
            "{} `{pattern}` names no job list",
            consts::QUARANTINE_JOBS_ENV
        );
    };
    let mut jobs = BTreeSet::new();
    let mut job = String::new();
    let mut chars = names.chars();
    while let Some(char) = chars.next() {
        match char {
            '\\' => match chars.next() {
                Some(escaped) => job.push(escaped),
                None => bail!(
                    "{} `{pattern}` ends in an escape",
                    consts::QUARANTINE_JOBS_ENV
                ),
            },
            '|' => {
                jobs.insert(std::mem::take(&mut job));
            }
            char => job.push(char),
        }
    }
    jobs.insert(job);
    if jobs.contains("") {
        bail!(
            "{} `{pattern}` names an empty job",
            consts::QUARANTINE_JOBS_ENV
        );
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_list_reads_back_the_jobs_it_names() {
        let jobs = ["apple:test", "linux:lint", "odd|name.x"].map(str::to_owned);
        let pattern = pattern(&jobs);

        assert_eq!(pattern, r"/^(apple:test|linux:lint|odd\|name\.x)$/");
        assert_eq!(jobs_of(&pattern).unwrap(), BTreeSet::from(jobs));
    }

    #[test]
    fn a_pattern_that_is_no_job_list_is_refused() {
        for pattern in ["apple:test", "/^()$/", r"/^(apple\)$/"] {
            assert!(jobs_of(pattern).is_err(), "{pattern}");
        }
    }
}
