use arc_swap::ArcSwap;
use iced::window::Id;
use kithara::{
    host::HostConfig,
    platform::{
        CancelToken,
        sync::Arc,
        thread,
        time::{Duration, Instant},
        tokio::sync::mpsc::{self, UnboundedReceiver},
    },
    ui::render::{ControlAction, ReadValue, Reads, UiEvent, Walk},
};

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
    #[cfg(not(feature = "broadcast"))]
    states: Vec<Arc<kithara::platform::sync::Mutex<crate::state::UiState>>>,
}

impl Rig {
    pub(crate) const DEADLINE: Duration = Duration::from_secs(2);

    pub(crate) fn applied_seq(&self) -> u64 {
        self.snapshots.load().applied_seq
    }

    fn build(
        config: &AppConfig,
        mut host: AppHost,
        broadcast: Broadcaster,
        model: impl FnMut(&Deck) -> StateController,
    ) -> Self {
        let decks: Vec<Deck> = (0..2)
            .map(|index| {
                Deck::build(DeckId(index), config, &mut host).expect("host accepts the test deck")
            })
            .collect();
        let queues = decks
            .iter()
            .map(|deck| deck.queue.control().clone())
            .collect();
        let deck_tokens = decks.iter().map(Deck::cancel_child).collect();
        let (sender, commands) = mpsc::unbounded_channel();
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let boot = test_fixture::boot(config, Arc::clone(&snapshots), sender);
        let engine = Engine::new(
            DeckSet::new(host, decks),
            config.clone(),
            broadcast,
            Arc::clone(&snapshots),
            model,
        );
        let ui = Kithara::mounted(boot, Id::unique());
        Self {
            snapshots,
            engine,
            ui,
            commands,
            queues,
            deck_tokens,
            #[cfg(not(feature = "broadcast"))]
            states: Vec::new(),
            shutdown: config.shutdown.clone(),
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

    #[cfg(not(feature = "broadcast"))]
    pub(crate) fn offline() -> Self {
        Self::offline_with(|_| {})
    }

    #[cfg(not(feature = "broadcast"))]
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
            |deck| {
                let queue = deck.queue.control().clone();
                let state = Arc::new(kithara::platform::sync::Mutex::new(
                    crate::state::UiState::new(&queue),
                ));
                states.push(Arc::clone(&state));
                crate::state::test_fixture::controller_on(
                    queue,
                    Arc::clone(&deck.timestretch),
                    deck.cancel_child(),
                    state,
                )
            },
        );
        rig.states = states;
        rig
    }

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
        Self::realtime_with(&config, Broadcaster::new(broadcast))
    }

    pub(crate) fn pump(&mut self) -> Vec<u64> {
        let mut applied = Vec::new();
        while let Ok(envelope) = self.commands.try_recv() {
            applied.push(envelope.seq);
            self.engine.apply(envelope);
        }
        self.engine.publish();
        applied
    }

    #[cfg(not(feature = "broadcast"))]
    pub(crate) fn realtime() -> Self {
        Self::realtime_with(
            &test_fixture::config(),
            Broadcaster::new(AppBroadcastConfig::default()),
        )
    }

    fn realtime_with(config: &AppConfig, broadcast: Broadcaster) -> Self {
        let host = AppHost::new(HostConfig::builder().build()).expect("test host");
        let (analysis, _) = AnalysisHandle::channel();
        Self::build(config, host, broadcast, |deck| {
            StateController::new(
                deck.queue.control().clone(),
                Arc::clone(&deck.timestretch),
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
        self.message(Message::Ui(UiEvent::Control {
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

    pub(crate) fn until(
        &mut self,
        what: &str,
        within: Duration,
        mut step: impl FnMut(&mut Self),
        mut done: impl FnMut(&mut Self) -> bool,
    ) {
        let deadline = Instant::now() + within;
        loop {
            step(self);
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: not within {within:?}");
            thread::paced_backoff(Duration::from_millis(5));
        }
    }
}
