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
    deck::DeckSet,
    pools::AppHost,
    sources::build_sources,
    state::StateController,
    wave_cache::{AnalysisPersistence, persistence::AnalysisPersistenceConfig},
};

pub(crate) fn build(
    config: AppConfig,
    host: AppHost,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
) -> Result<Engine, EngineError> {
    let base_worker = config
        .base_worker
        .clone()
        .ok_or(EngineError::Missing("base worker"))?;

    let mut dispatcher = DispatcherConfig::builder()
        .name("kithara-analysis-persistence")
        .build();
    dispatcher.apply(config.dispatcher.clone());

    let persistence = AnalysisPersistence::new(
        AnalysisPersistenceConfig::builder()
            .worker(base_worker)
            .pools(config.worker.pools().clone())
            .queue_capacity(NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN))
            .chunk_duration(Duration::from_secs(u64::from(
                config.analysis_chunk_seconds.get(),
            )))
            .dispatcher(dispatcher)
            .task(TaskConfig::new())
            .build(),
    )?;

    let (analysis, handle) = AnalysisService::new(&config, persistence, config.shutdown.child());
    task::spawn(analysis.run());

    let mut session = DeckSet::new(&config, host, |queue, cancel, grids| {
        StateController::new(
            queue.control().clone(),
            cancel.child(),
            handle.clone(),
            grids,
        )
    })?;
    session.send_levels()?;

    if let Some(first) = session.records().first() {
        first.queue().set_tracks(build_sources(&config))?;
    }

    let broadcast = config
        .broadcast
        .clone()
        .map(Broadcaster::new)
        .ok_or(EngineError::Missing("broadcast service"))?;

    Ok(Engine::new(session, config, broadcast, snapshots, handle))
}
