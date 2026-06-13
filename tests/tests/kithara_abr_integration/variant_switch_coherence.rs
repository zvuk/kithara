use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use kithara_abr::{AbrController, AbrMode, AbrSettings, AbrState, ThroughputEstimator};
use kithara_events::{
    AbrEvent, AbrProgressSnapshot, BandwidthSource, DEFAULT_EVENT_BUS_CAPACITY, Event, EventBus,
    VariantDuration, VariantIndex, VariantInfo,
};
use kithara_platform::{
    CancelToken,
    time::{Duration, Duration as StdDuration},
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

    let reader_bg = Arc::clone(&reader);
    let committed_bg = Arc::clone(&committed);
    let ticker = kithara_platform::tokio::task::spawn(async move {
        for _ in 0..30 {
            reader_bg.fetch_add(1, Ordering::AcqRel);
            committed_bg.fetch_add(1, Ordering::AcqRel);
            kithara_platform::time::sleep(StdDuration::from_millis(20)).await;
        }
    });

    let deadline = kithara_platform::time::Instant::now() + StdDuration::from_millis(600);

    let mut saw_incoherence = AtomicBool::new(false);
    while kithara_platform::time::Instant::now() < deadline {
        let timeout =
            kithara_platform::time::timeout(StdDuration::from_millis(30), rx.recv()).await;
        match timeout {
            Ok(Ok(Event::Abr(AbrEvent::Incoherence { .. }))) => {
                saw_incoherence = AtomicBool::new(true);
                break;
            }
            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => continue,
        }
    }
    ticker.abort();

    assert!(
        !*saw_incoherence.get_mut(),
        "Incoherence fired during a healthy switch"
    );
}
