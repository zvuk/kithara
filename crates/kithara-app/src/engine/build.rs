use std::num::NonZeroUsize;

use arc_swap::ArcSwap;
use kithara::{
    platform::{sync::Arc, time::Duration, tokio::task},
    worker::{DispatcherConfig, TaskConfig},
};

use super::{Engine, EngineError, EngineSnapshot};
use crate::{
    analysis::AnalysisService,
    broadcast::Broadcaster,
    config::AppConfig,
    deck::{Deck, DeckId, DeckSet},
    pools::AppHost,
    sources::build_sources,
    state::StateController,
    wave_cache::{AnalysisPersistence, persistence::AnalysisPersistenceConfig},
};

pub(crate) fn build(
    config: AppConfig,
    mut host: AppHost,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
) -> Result<Engine, EngineError> {
    let decks = vec![
        Deck::build(DeckId(0), &config, &mut host)?,
        Deck::build(DeckId(1), &config, &mut host)?,
    ];
    let mut session = DeckSet::new(host, decks);
    session.commit(session.mix().clone())?;

    let base_worker = config
        .base_worker
        .clone()
        .ok_or(EngineError::Missing("base worker"))?;
    let mut dispatcher = DispatcherConfig::builder()
        .name("kithara-analysis-persistence")
        .build();
    dispatcher.apply(config.dispatcher.clone());
    let persistence = AnalysisPersistence::new(AnalysisPersistenceConfig::new(
        base_worker,
        config.worker.pools().clone(),
        NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
        Duration::from_secs(u64::from(config.analysis_chunk_seconds.get())),
        dispatcher,
        TaskConfig::new(),
    ))?;
    let (analysis, handle) = AnalysisService::new(&config, persistence, config.shutdown.child());
    task::spawn(analysis.run());

    if let Some(first) = session.decks().first() {
        first.queue.set_tracks(build_sources(&config));
    }
    let broadcast = config
        .broadcast
        .clone()
        .map(Broadcaster::new)
        .ok_or(EngineError::Missing("broadcast service"))?;
    Ok(Engine::new(session, config, broadcast, snapshots, |deck| {
        StateController::new(
            deck.queue.control().clone(),
            Arc::clone(&deck.timestretch),
            deck.cancel_child(),
            handle.clone(),
        )
    }))
}
