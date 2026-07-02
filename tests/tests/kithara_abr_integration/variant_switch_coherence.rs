use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use kithara_abr::{
    AbrController, AbrDecision, AbrMode, AbrReason, AbrSettings, AbrState, ThroughputEstimator,
};
use kithara_events::{
    AbrEvent, AbrProgressSnapshot, BandwidthSource, DEFAULT_EVENT_BUS_CAPACITY, Event, EventBus,
    VariantDuration, VariantIndex, VariantInfo,
};
use kithara_platform::{
    CancelToken,
    time::{Duration, Duration as StdDuration, Instant},
    tokio,
};
use kithara_test_utils::kithara;

fn fast_settings() -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(2_000_000)
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .incoherence_deadline(Duration::from_millis(250))
        .build()
}

struct AdvancingPeer {
    state: Arc<AbrState>,
    reader: Arc<AtomicUsize>,
    committed: Arc<AtomicUsize>,
    durations: Vec<Duration>,
    variants: Vec<VariantInfo>,
}

impl kithara_abr::Abr for AdvancingPeer {
    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.clone()
    }
    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }
    fn progress(&self) -> Option<AbrProgressSnapshot> {
        let reader = self.reader.load(Ordering::Acquire);
        let committed = self.committed.load(Ordering::Acquire);
        let r = reader.min(self.durations.len());
        let c = committed.min(self.durations.len());
        Some(AbrProgressSnapshot {
            reader_playback_time: self.durations[..r].iter().copied().sum(),
            download_head_playback_time: self.durations[..c].iter().copied().sum(),
        })
    }
}

#[kithara::test(tokio)]
async fn normal_switch_keeps_reader_advancing_no_incoherence() {
    let bus = EventBus::new(DEFAULT_EVENT_BUS_CAPACITY);
    let mut rx = bus.subscribe();

    let durations: Vec<Duration> = (0..40).map(|_| Duration::from_secs(2)).collect();
    let variants: Vec<VariantInfo> = [300_000u64, 900_000, 3_000_000]
        .iter()
        .enumerate()
        .map(|(i, bps)| VariantInfo {
            variant_index: VariantIndex::new(i),
            bandwidth_bps: Some(*bps),
            duration: VariantDuration::Segmented(durations.clone()),
            name: None,
            codecs: None,
            container: None,
        })
        .collect();

    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    let reader = Arc::new(AtomicUsize::new(0));
    let committed = Arc::new(AtomicUsize::new(3));
    let peer: Arc<dyn kithara_abr::Abr> = Arc::new(AdvancingPeer {
        state: Arc::clone(&state),
        reader: Arc::clone(&reader),
        committed: Arc::clone(&committed),
        durations,
        variants,
    });

    let controller = AbrController::with_estimator(
        fast_settings(),
        Arc::new(ThroughputEstimator::new()) as Arc<_>,
        CancelToken::never(),
    );
    let handle = controller.register(&peer).with_bus(bus);

    for _ in 0..30 {
        controller.record_bandwidth(
            handle.peer_id(),
            512 * 1024,
            Duration::from_millis(20),
            BandwidthSource::Network,
        );
    }

    // Arm the production incoherence watcher: notify_commit emits VariantApplied
    // and schedules the check after `incoherence_deadline` (250ms virtual). The
    // negative assertion below is only non-vacuous once a watcher is actually
    // armed against this switch.
    let reader_pt_at_switch = peer
        .progress()
        .expect("advancing peer reports progress")
        .reader_playback_time;
    let switch_at = Instant::now();
    let decision = AbrDecision::UpSwitch {
        from: VariantIndex::new(0),
        to: VariantIndex::new(2),
        reason: AbrReason::UpSwitch,
    };
    handle.notify_commit(decision, 0, reader_pt_at_switch, switch_at);

    // Drive the reader forward (healthy switch): once the reader advances past
    // `reader_pt_at_switch`, the watcher's check finds the reader unstuck and
    // stays silent. Advancing it synchronously makes the coherent outcome a
    // fact, not a wall-clock gamble.
    let reader_bg = Arc::clone(&reader);
    let committed_bg = Arc::clone(&committed);
    let ticker = tokio::task::spawn(async move {
        for _ in 0..30 {
            reader_bg.fetch_add(1, Ordering::AcqRel);
            committed_bg.fetch_add(1, Ordering::AcqRel);
            kithara_platform::time::sleep(StdDuration::from_millis(20)).await;
        }
    });

    let mut saw_incoherence = AtomicBool::new(false);
    // Wait on the real state: the reader has advanced past the switch mark AND
    // the watcher's virtual window (incoherence_deadline) has fully elapsed, so
    // it has had its chance to fire. Each tick uses the virtual platform clock
    // (advances under flash); the watchdog PANICS on genuine non-progress and
    // never lets the assertion pass on timeout.
    let window = fast_settings().incoherence_deadline;
    let watchdog = Instant::now() + StdDuration::from_secs(5);
    loop {
        if let Ok(Event::Abr(AbrEvent::Incoherence { .. })) = rx.try_recv() {
            saw_incoherence = AtomicBool::new(true);
            break;
        }
        let reader_advanced = peer
            .progress()
            .is_some_and(|p| p.reader_playback_time > reader_pt_at_switch);
        let window_elapsed = switch_at.elapsed() > window;
        if reader_advanced && window_elapsed {
            break;
        }
        assert!(
            Instant::now() < watchdog,
            "watcher window never closed: reader did not advance past the \
             switch mark within the incoherence deadline"
        );
        kithara_platform::time::sleep(StdDuration::from_millis(5)).await;
    }
    ticker.abort();

    assert!(
        !*saw_incoherence.get_mut(),
        "Incoherence fired during a healthy switch"
    );
}
