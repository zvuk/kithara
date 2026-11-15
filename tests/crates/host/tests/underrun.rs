use std::collections::BTreeMap;

use kithara::platform::tokio::sync::broadcast::{Receiver, error::TryRecvError};
use kithara_integration_tests::offline::OfflinePlayer;
use kithara_render::bridge::{DeckEvent, RtMetricsSnapshot};

pub(super) struct Underruns {
    receiver: Receiver<DeckEvent>,
    baseline: RtMetricsSnapshot,
    edges: Vec<DeckEvent>,
}

impl Underruns {
    pub(super) fn new(harness: &OfflinePlayer) -> Self {
        Self {
            receiver: harness.subscribe_underruns(),
            baseline: harness.metrics(),
            edges: Vec::new(),
        }
    }

    pub(super) fn checkpoint(&mut self, metrics: RtMetricsSnapshot, complete: bool, label: &str) {
        assert_eq!(
            metrics.event_overflows(),
            self.baseline.event_overflows(),
            "mixer event overflow"
        );
        loop {
            match self.receiver.try_recv() {
                Ok(event) => self.edges.push(event),
                Err(TryRecvError::Empty) => break,
                Err(error) => panic!("incomplete raw underrun observation: {error}"),
            }
        }
        let delta = metrics
            .underruns()
            .checked_sub(self.baseline.underruns())
            .expect("underrun counter is monotonic");
        tracing::info!(target: "kithara_test::host_rate_telemetry", label, delta, complete, edges = ?self.edges, "raw mixer underrun checkpoint");
        assert_eq!(
            validate(&self.edges, delta, complete),
            Ok(()),
            "{label}: {:?}",
            self.edges
        );
    }
}

fn validate(edges: &[DeckEvent], delta: u64, complete: bool) -> Result<(), &'static str> {
    let mut open = BTreeMap::new();
    let mut starts = 0_u64;
    for edge in edges {
        match edge {
            DeckEvent::UnderrunStarted { slot, item, at, .. } => {
                if open.insert((*slot, *item), *at).is_some() {
                    return Err("duplicate underrun start");
                }
                starts += 1;
            }
            DeckEvent::UnderrunEnded {
                slot,
                item,
                at,
                frames,
                ..
            } => {
                let start = open
                    .remove(&(*slot, *item))
                    .ok_or("underrun end has no start")?;
                if *at < start || *frames == 0 {
                    return Err("invalid underrun interval");
                }
            }
            _ => return Err("unexpected raw underrun event"),
        }
    }
    if starts != delta {
        return Err("raw starts disagree with the owner metric delta");
    }
    if complete && !open.is_empty() {
        return Err("missing underrun end");
    }
    Ok(())
}

#[kithara_test_utils::kithara::test]
fn rejects_missing_duplicate_and_invalid_edges_and_metric_disagreement() {
    use kithara::{events::TrackId, signal::SessionFrame};
    use kithara_render::bridge::Slot;

    let start = DeckEvent::UnderrunStarted {
        slot: Slot::new(0),
        item: TrackId(1),
        at: SessionFrame::new(100),
        position: None,
    };
    let end = DeckEvent::UnderrunEnded {
        slot: Slot::new(0),
        item: TrackId(1),
        at: SessionFrame::new(164),
        position: None,
        frames: 64,
        recovered: true,
    };
    assert_eq!(validate(&[start], 1, false), Ok(()));
    assert_eq!(validate(&[start, end], 1, true), Ok(()));
    for (edges, delta) in [
        (vec![start], 1),
        (vec![end], 0),
        (vec![start, start], 2),
        (vec![start, end, end], 1),
        (vec![start, end], 0),
        (
            vec![
                start,
                DeckEvent::UnderrunEnded {
                    slot: Slot::new(0),
                    item: TrackId(1),
                    at: SessionFrame::new(99),
                    position: None,
                    frames: 64,
                    recovered: false,
                },
            ],
            1,
        ),
        (
            vec![
                start,
                DeckEvent::UnderrunEnded {
                    slot: Slot::new(0),
                    item: TrackId(1),
                    at: SessionFrame::new(164),
                    position: None,
                    frames: 0,
                    recovered: true,
                },
            ],
            1,
        ),
    ] {
        assert!(validate(&edges, delta, true).is_err());
    }
}
