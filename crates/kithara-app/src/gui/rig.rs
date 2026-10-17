use arc_swap::ArcSwap;
use iced::window::Id;
use kithara::{
    host::HostConfig,
    platform::{
        CancelToken,
        sync::Arc,
        thread,
        time::{Duration, WallInstant},
        tokio::sync::{
            mpsc::{self, UnboundedReceiver},
            watch,
        },
    },
    ui::render::{ControlAction, Published, ReadValue, Reads, Walk},
};
use kithara_test_utils::{clock::real_clock_verdict, kithara as test_attrs};

use super::{app::Kithara, message::Message, reads::ReadRoot, test_fixture, update::update};
use crate::{
    analysis::AnalysisHandle,
    broadcast::Broadcaster,
    config::{AppBroadcastConfig, AppConfig},
    deck::{Deck, DeckId, DeckSet},
    engine::{Engine, EngineSnapshot, Envelope},
    pools::{AppHost, AppQueueControl},
    state::StateController,
};

pub(crate) struct Rig {
    pub(crate) snapshots: Arc<ArcSwap<EngineSnapshot>>,
    pub(crate) engine: Engine,
    pub(crate) ui: Kithara,
    pub(crate) commands: UnboundedReceiver<Envelope>,
    pub(crate) queues: Vec<AppQueueControl>,
    pub(crate) deck_tokens: Vec<CancelToken>,
    pub(crate) shutdown: CancelToken,
    pub(crate) rendered_frames: u64,
    #[cfg(not(feature = "broadcast"))]
    states: Vec<Arc<kithara::platform::sync::Mutex<crate::state::UiState>>>,
    _runtime: kithara::platform::tokio::runtime::Runtime,
}

impl Rig {
    /// Budget for a wait the engine answers on its own: a command lands, a
    /// snapshot publishes, a deck reports what it opened.
    pub(crate) const DEADLINE: Duration = Duration::from_secs(2);

    /// Budget for a wait that only ends once a track reaches its own end.
    ///
    /// Playing the track out IS the wait, so the audio is not slack inside
    /// [`DEADLINE`](Self::DEADLINE) — a fixture as long as that constant
    /// leaves the engine no time at all to answer, and the assert fires on a
    /// deadline no run could have met. The track's own length carries the
    /// budget and `DEADLINE` is what remains for the answer.
    pub(crate) const fn playout(track: Duration) -> Duration {
        track.saturating_add(Self::DEADLINE)
    }

    pub(crate) fn applied_seq(&self) -> u64 {
        self.snapshots.load().applied_seq
    }

    #[test_attrs::hang_watchdog(timeout = Duration::from_secs(2))]
    fn build(
        config: &AppConfig,
        mut host: AppHost,
        broadcast: Broadcaster,
        analysis: AnalysisHandle,
        model: impl FnMut(&Deck) -> StateController,
    ) -> Self {
        let decks: Vec<Deck> = (0..2)
            .map(|index| {
                Deck::build(DeckId(index), config, &mut host).expect("host accepts the test deck")
            })
            .collect();
        if config.eq_bands > 3 {
            for deck in &decks {
                hang_tick!();
                hang_park!(|_| {
                    deck.queue
                        .control()
                        .set_eq_layout(
                            crate::deck::EqMode::ThreeBand
                                .layout(&[kithara::effects::GainDb::default(); 3]),
                        )
                        .expect("the fixture starts in three-band mode");
                });
                hang_reset!();
            }
            crate::mock::render_host(&mut host, 0..128, &config.shutdown)
                .expect("the initial EQ layout renders");
            hang_reset!();
        }
        let queues = decks
            .iter()
            .map(|deck| deck.queue.control().clone())
            .collect();
        let deck_tokens = decks.iter().map(Deck::cancel_child).collect();
        let (sender, commands) = mpsc::unbounded_channel();
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let runtime = test_fixture::runtime();
        let boot = test_fixture::boot(
            runtime.handle(),
            config,
            Arc::clone(&snapshots),
            sender,
            Vec::new(),
        );
        let engine = Engine::new(
            DeckSet::new(host, decks),
            config.clone(),
            broadcast,
            Arc::clone(&snapshots),
            analysis,
            model,
        );
        let ui = Kithara::mounted(boot, Id::unique());
        Self {
            _runtime: runtime,
            snapshots,
            engine,
            ui,
            commands,
            queues,
            deck_tokens,
            #[cfg(not(feature = "broadcast"))]
            states: Vec::new(),
            shutdown: config.shutdown.clone(),
            rendered_frames: if config.eq_bands > 3 { 128 } else { 0 },
        }
    }

    pub(crate) fn close_window(&mut self) {
        assert_eq!(
            update(&mut self.ui, Message::WindowCloseRequested).units(),
            1
        );
    }

    pub(crate) fn flag(&self, key: &str) -> bool {
        let root = ReadRoot::new(&self.ui);
        match Walk::new(&root).get(key) {
            Some(ReadValue::Bool(value)) => value,
            other => panic!("{key} draws {other:?}, not a flag"),
        }
    }

    pub(crate) fn frame(&mut self) {
        self.message(Message::Tick);
    }

    pub(crate) fn message(&mut self, message: Message) {
        assert_eq!(update(&mut self.ui, message).units(), 0);
    }

    /// Uses an offline host because broadcast/window tests need a measured stream,
    /// without depending on a device.
    #[cfg(feature = "broadcast")]
    pub(crate) fn on_air() -> Self {
        use kithara::worker::{Worker, WorkerConfig};

        let config = test_fixture::config();
        let broadcast = AppBroadcastConfig::builder(
            Worker::new(WorkerConfig::new()),
            config.worker.pools().clone(),
        )
        .cancel(config.shutdown.child())
        .build();
        let host = AppHost::new(HostConfig::offline(config.worker.pools().clone()).build())
            .expect("test host");
        Self::with_host(&config, host, Broadcaster::new(broadcast))
    }

    #[test_attrs::hang_watchdog(timeout = Duration::from_secs(2))]
    pub(crate) fn pump(&mut self) -> Vec<u64> {
        let mut applied = Vec::new();
        let _ = __hang_detector.remaining();
        hang_tick!();
        while let Ok(envelope) = self.commands.try_recv() {
            applied.push(envelope.seq);
            self.engine.apply(envelope);
            let remaining = __hang_detector.remaining();
            hang_tick!();
            assert!(
                !remaining.is_zero(),
                "app fixture command application exceeded its idle budget"
            );
            hang_reset!();
            let _ = __hang_detector.remaining();
        }
        crate::engine::render_fixture(self, 128);
        self.engine.tick();
        self.engine.publish();
        let remaining = __hang_detector.remaining();
        hang_tick!();
        assert!(
            !remaining.is_zero(),
            "app fixture render and publication exceeded its idle budget"
        );
        applied
    }

    #[cfg(not(feature = "broadcast"))]
    pub(crate) fn playback() -> Self {
        let config = test_fixture::config();
        let host = AppHost::new(HostConfig::offline(config.worker.pools().clone()).build())
            .expect("test host");
        Self::with_host(
            &config,
            host,
            Broadcaster::new(AppBroadcastConfig::default()),
        )
    }

    fn with_host(config: &AppConfig, host: AppHost, broadcast: Broadcaster) -> Self {
        let (analysis, _) = AnalysisHandle::channel(watch::channel(Arc::default()).1);
        Self::build(config, host, broadcast, analysis.clone(), |deck| {
            StateController::new(
                deck.queue.control().clone(),
                deck.cancel_child(),
                analysis.clone(),
            )
        })
    }

    #[cfg(not(feature = "broadcast"))]
    pub(crate) fn scalar(&self, key: &str) -> f64 {
        let root = ReadRoot::new(&self.ui);
        match Walk::new(&root).get(key) {
            Some(ReadValue::Scalar(value)) => value,
            other => panic!("{key} draws {other:?}, not a scalar"),
        }
    }

    pub(crate) fn send(&mut self, path: &str, action: ControlAction) {
        self.message(Message::Ui(Published::Gesture {
            action,
            path: path.to_owned(),
        }));
    }

    #[cfg(not(feature = "broadcast"))]
    pub(crate) fn shows(&mut self, id: DeckId, change: impl FnOnce(&mut crate::state::UiState)) {
        change(&mut self.states[id.0].lock());
        self.engine.publish();
        self.ui.refresh();
    }

    #[cfg(not(feature = "broadcast"))]
    pub(crate) fn text(&self, key: &str) -> Option<String> {
        let root = ReadRoot::new(&self.ui);
        match Walk::new(&root).get(key) {
            Some(ReadValue::Text(value)) => Some(value.to_owned()),
            _ => None,
        }
    }

    /// Drive `step` until `done`, giving up after `within`.
    ///
    /// The wall-clock budget bounds fixture I/O as well as engine work. A
    /// playing deck's position or an applied command resets its idle budget.
    #[test_attrs::flash(io)]
    #[test_attrs::hang_watchdog(timeout = Duration::from_secs(2))]
    pub(crate) fn until(
        &mut self,
        what: &str,
        within: Duration,
        mut step: impl FnMut(&mut Self),
        mut done: impl FnMut(&mut Self) -> bool,
    ) {
        let deadline = WallInstant::now() + within;
        let started_real = WallInstant::now();
        let progress = |rig: &Self| {
            (
                rig.applied_seq(),
                rig.queues
                    .iter()
                    .map(|queue| {
                        (
                            queue.current_index(),
                            queue.position_seconds(),
                            queue.duration_seconds(),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        loop {
            let _ = __hang_detector.remaining();
            let before = progress(self);
            crate::engine::render_fixture(self, 1024);
            self.engine.tick();
            self.engine.publish();
            step(self);
            let reached = done(self);
            let after = progress(self);
            let remaining = __hang_detector.remaining();
            hang_tick!();
            assert!(
                !remaining.is_zero(),
                "{what}: app fixture step exceeded its idle budget"
            );
            assert!(
                WallInstant::now() < deadline,
                "{what}: not within {within:?} | {}",
                real_clock_verdict(started_real, within)
            );
            if reached {
                return;
            }
            if before != after {
                tracing::debug!(what, ?after, "the app fixture advanced");
                hang_reset!();
            }
            thread::paced_backoff(__hang_detector.remaining().min(Duration::from_millis(5)));
        }
    }
}

#[cfg(not(feature = "broadcast"))]
impl Rig {
    pub(crate) fn offline() -> Self {
        Self::offline_with(|_| {})
    }

    pub(crate) fn offline_with(configure: impl FnOnce(&mut AppConfig)) -> Self {
        let mut config = test_fixture::config();
        configure(&mut config);
        let host = AppHost::new(HostConfig::offline(config.worker.pools().clone()).build())
            .expect("test host");
        let mut states = Vec::new();
        let mut rig = Self::build(
            &config,
            host,
            Broadcaster::new(AppBroadcastConfig::default()),
            AnalysisHandle::channel(watch::channel(Arc::default()).1).0,
            |deck| {
                let queue = deck.queue.control().clone();
                let state = Arc::new(kithara::platform::sync::Mutex::new(
                    crate::state::UiState::new(&queue),
                ));
                states.push(Arc::clone(&state));
                crate::state::test_fixture::controller_on(queue, deck.cancel_child(), state)
            },
        );
        rig.states = states;
        rig
    }

    /// Drops a library row playing `source` on deck `deck` and applies the load.
    pub(crate) fn drop_on(&mut self, deck: &str, source: &str) {
        self.send(
            &format!("deck-{deck}/drop"),
            ControlAction::Record(test_fixture::dragged(source)),
        );
        self.pump();
    }
}
