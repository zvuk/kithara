use kithara::platform::time::Duration;

use crate::usdt_trace::ProbeEvent;

pub fn assert_committed_reached(records: &[ProbeEvent], min: Duration) {
    let reached = records
        .iter()
        .filter(|record| record.probe == "write_playhead")
        .filter_map(|record| record.field("committed_ns"))
        .max()
        .is_some_and(|position| position >= u64::try_from(min.as_nanos()).unwrap_or(u64::MAX));
    assert!(reached, "committed playhead did not reach {min:?}");
}

pub fn assert_no_committed_swallow(records: &[ProbeEvent], maximum_step: Duration) {
    let mut previous = None;
    let maximum = u64::try_from(maximum_step.as_nanos()).unwrap_or(u64::MAX);
    for record in records
        .iter()
        .filter(|record| record.probe == "write_playhead")
    {
        let Some(current) = record.field("committed_ns") else {
            continue;
        };
        if let Some(previous) = previous {
            assert!(
                current.saturating_sub(previous) <= maximum,
                "committed playhead jumped from {previous} ns to {current} ns"
            );
        }
        previous = Some(current);
    }
    assert!(previous.is_some(), "zero write_playhead USDT records");
}
