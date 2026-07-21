use kithara_audio::SeekOutcome;
use kithara_platform::{maybe_send::BoxFuture, sync::Arc, time::Duration};

use super::{
    component::PlayerComponentBox,
    core::{MultiPlayerCore, TopologyRevision},
    member::Member,
    session,
};
use crate::{
    api::{Player, PlayerCollector, PlayerComponent, SessionBeat, SessionSeek, StartAt, Tempo},
    error::PlayError,
};

/// One composition of heterogeneous player components.
///
/// The group owns every registered value. Callers retain only typed
/// [`Member<T>`] routes, so member controls always pass through the current
/// root composition.
#[derive(Default)]
pub struct MultiPlayer {
    pub(super) core: Arc<MultiPlayerCore>,
}

impl MultiPlayer {
    /// Register and consume one component, returning its routed typed handle.
    pub fn register<T>(&self, component: T) -> Result<Member<T>, PlayError>
    where
        PlayerComponentBox: From<T>,
    {
        let member_id = self.core.register(PlayerComponentBox::from(component))?;
        Ok(Member::new(member_id, Arc::downgrade(&self.core)))
    }

    /// Commit one checked tempo transaction across every canonical player
    /// leaf in this composition.
    pub fn set_session_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
        let _transaction = self.core.begin_tempo()?;
        let players = self.core.collect_players();
        let players: Vec<_> = players.iter().map(AsRef::as_ref).collect();
        session::set_session_tempo(&players, tempo)
    }

    /// Current topology revision.
    #[must_use]
    pub fn topology_revision(&self) -> TopologyRevision {
        self.core.topology_revision()
    }
}

impl Player for MultiPlayer {
    fn duration_seconds(&self) -> Option<f64> {
        let mut minimum = None;
        for component in self.core.components() {
            let duration = component.component.duration_seconds()?;
            minimum = Some(minimum.map_or(duration, |current: f64| current.min(duration)));
        }
        minimum
    }

    fn pause(&self) -> Result<(), PlayError> {
        let _transaction = self.core.begin_control()?;
        for component in self.core.components() {
            component.component.pause()?;
        }
        Ok(())
    }

    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        let _transaction = self.core.begin_seek()?;
        let components = self.core.components();
        if components.is_empty() {
            return Err(PlayError::NotReady);
        }
        let players = MultiPlayerCore::collect_players_from(&components);
        if players
            .iter()
            .any(|player| player.core.items.current_has_binding())
        {
            return Err(PlayError::BoundTrackSeekRequiresSessionTransport);
        }
        let target_seconds = seconds.max(0.0);
        let target = Duration::from_secs_f64(target_seconds);
        let shortest = components
            .iter()
            .try_fold(f64::INFINITY, |shortest, component| {
                component
                    .component
                    .duration_seconds()
                    .map(|duration| shortest.min(duration))
                    .ok_or(PlayError::NotReady)
            })?;
        if target_seconds >= shortest {
            return Ok(SeekOutcome::PastEof {
                target,
                duration: Duration::from_secs_f64(shortest),
            });
        }
        let mut outcome = None;
        for component in components {
            outcome = Some(component.component.seek_seconds(target_seconds)?);
        }
        outcome.ok_or(PlayError::NotReady)
    }

    fn start_at(&self, start: StartAt) -> Result<(), PlayError> {
        let _transaction = self.core.begin_control()?;
        for component in self.core.components() {
            component.component.start_at(start)?;
        }
        Ok(())
    }
}

impl PlayerComponent for MultiPlayer {
    fn collect_players(&self, collector: &mut PlayerCollector<'_>) {
        collector.push_composition(Arc::clone(&self.core));
        collector.extend(self.core.collect_players());
    }
}

impl SessionSeek for MultiPlayer {
    fn seek_session(&self, target: SessionBeat) -> BoxFuture<'_, Result<(), PlayError>> {
        Box::pin(seek_core(&self.core, target))
    }
}

pub(super) async fn seek_core(
    core: &Arc<MultiPlayerCore>,
    target: SessionBeat,
) -> Result<(), PlayError> {
    let _transaction = core.begin_seek()?;
    let players = core.collect_players();
    let players: Vec<_> = players.iter().map(AsRef::as_ref).collect();
    session::seek_session(&players, target).await
}
