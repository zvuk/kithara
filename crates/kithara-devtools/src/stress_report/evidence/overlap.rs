use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use super::{MAX_SIGNATURE_EXAMPLES, attempt::AttemptKey, duration_ms, parse_timestamp_ms};
use crate::junit::CaseTiming;

#[derive(Debug)]
struct Interval {
    key: AttemptKey,
    test: String,
    end: u64,
    start: u64,
}

pub(super) fn for_targets(
    cases: &[CaseTiming],
    targets: &BTreeSet<AttemptKey>,
) -> BTreeMap<AttemptKey, BTreeSet<String>> {
    let mut intervals = cases.iter().filter_map(interval).collect::<Vec<Interval>>();
    intervals.sort_by_key(|interval| (interval.start, interval.end));

    let mut overlaps = intervals
        .iter()
        .filter(|interval| targets.contains(&interval.key))
        .map(|interval| (interval.key.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut expiry = BinaryHeap::<Reverse<(u64, usize)>>::new();
    let mut active = BTreeSet::<usize>::new();
    let mut active_failed = BTreeSet::<usize>::new();

    for current in 0..intervals.len() {
        while let Some(Reverse((end, index))) = expiry.peek().copied() {
            if end > intervals[current].start {
                break;
            }
            expiry.pop();
            active.remove(&index);
            active_failed.remove(&index);
        }

        if targets.contains(&intervals[current].key) {
            let row = overlaps.entry(intervals[current].key.clone()).or_default();
            for index in &active {
                if row.len() == MAX_SIGNATURE_EXAMPLES {
                    break;
                }
                if intervals[*index].key != intervals[current].key {
                    row.insert(intervals[*index].test.clone());
                }
            }
        }
        for index in &active_failed {
            if intervals[*index].key != intervals[current].key
                && let Some(row) = overlaps.get_mut(&intervals[*index].key)
                && row.len() < MAX_SIGNATURE_EXAMPLES
            {
                row.insert(intervals[current].test.clone());
            }
        }

        active.insert(current);
        if targets.contains(&intervals[current].key) {
            active_failed.insert(current);
        }
        expiry.push(Reverse((intervals[current].end, current)));
    }
    overlaps
}

fn interval(case: &CaseTiming) -> Option<Interval> {
    let iteration = case.iteration?;
    let start = case.timestamp.as_deref().and_then(parse_timestamp_ms)?;
    Some(Interval {
        key: AttemptKey {
            iteration,
            suite: case.suite.clone(),
            name: case.name.clone(),
        },
        test: format!("{} {}", case.suite, case.name),
        start,
        end: start.saturating_add(duration_ms(case.secs)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str, iteration: usize, failed: bool, start: &str, secs: f64) -> CaseTiming {
        CaseTiming {
            failed,
            flaky: false,
            secs,
            name: name.to_owned(),
            suite: "demo".to_owned(),
            iteration: Some(iteration),
            timestamp: Some(start.to_owned()),
            output: String::new(),
            output_truncated: false,
        }
    }

    #[test]
    fn sweep_joins_only_strictly_overlapping_intervals() {
        let cases = vec![
            case("failed-a", 0, true, "1970-01-01T00:00:00.000Z", 1.0),
            case("pass-b", 0, false, "1970-01-01T00:00:00.500Z", 0.2),
            case("failed-d", 0, true, "1970-01-01T00:00:00.600Z", 0.2),
            case("boundary-c", 0, false, "1970-01-01T00:00:01.000Z", 0.2),
        ];

        let targets = BTreeSet::from([
            AttemptKey {
                suite: "demo".to_owned(),
                name: "failed-a".to_owned(),
                iteration: 0,
            },
            AttemptKey {
                suite: "demo".to_owned(),
                name: "failed-d".to_owned(),
                iteration: 0,
            },
        ]);
        let overlaps = for_targets(&cases, &targets);

        assert_eq!(
            overlaps
                .get(&AttemptKey {
                    suite: "demo".to_owned(),
                    name: "failed-a".to_owned(),
                    iteration: 0,
                })
                .expect("failed-a"),
            &BTreeSet::from(["demo failed-d".to_owned(), "demo pass-b".to_owned()])
        );
        assert_eq!(
            overlaps
                .get(&AttemptKey {
                    suite: "demo".to_owned(),
                    name: "failed-d".to_owned(),
                    iteration: 0,
                })
                .expect("failed-d"),
            &BTreeSet::from(["demo failed-a".to_owned(), "demo pass-b".to_owned()])
        );
    }

    #[test]
    fn sweep_only_allocates_requested_target_rows() {
        let cases = vec![
            case("failed-a", 0, true, "1970-01-01T00:00:00.000Z", 1.0),
            case("failed-b", 0, true, "1970-01-01T00:00:00.100Z", 1.0),
        ];
        let only_a = AttemptKey {
            suite: "demo".to_owned(),
            name: "failed-a".to_owned(),
            iteration: 0,
        };

        let overlaps = for_targets(&cases, &BTreeSet::from([only_a.clone()]));

        assert_eq!(overlaps.len(), 1);
        assert_eq!(
            overlaps.get(&only_a),
            Some(&BTreeSet::from(["demo failed-b".to_owned()]))
        );
    }

    #[test]
    fn co_runner_examples_are_bounded() {
        let mut cases = (0..20)
            .map(|index| {
                case(
                    &format!("pass-{index}"),
                    0,
                    false,
                    "1970-01-01T00:00:00.000Z",
                    1.0,
                )
            })
            .collect::<Vec<_>>();
        cases.push(case("failed", 0, true, "1970-01-01T00:00:00.500Z", 0.1));
        let target = AttemptKey {
            suite: "demo".to_owned(),
            name: "failed".to_owned(),
            iteration: 0,
        };

        let overlaps = for_targets(&cases, &BTreeSet::from([target.clone()]));

        assert_eq!(
            overlaps.get(&target).expect("target").len(),
            MAX_SIGNATURE_EXAMPLES
        );
    }
}
