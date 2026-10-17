use kithara_play::{PlayError, PlayerConfig, TrackFactory};

use crate::{LinkConfig, Linked, LinkedConfig, SyncMode, TempoTrajectory};

/// Builds every queue track with the deck's mode and latest Host trajectory.
pub struct LinkedFactory<F> {
    inner: F,
    config: LinkConfig,
    synced: bool,
    host: TempoTrajectory,
}

impl<F> LinkedFactory<F> {
    /// Decorates a track factory with synchronization initially off.
    #[must_use]
    pub fn new(config: LinkedConfig<F>) -> Self {
        let LinkedConfig {
            inner,
            settings: config,
            trajectory: host,
        } = config;
        Self {
            inner,
            config,
            synced: false,
            host,
        }
    }

    /// Sets the mode inherited by subsequent tracks, without sending a command.
    pub fn set_synced(&mut self, synced: bool) {
        self.synced = synced;
    }

    /// Whether subsequent tracks inherit synchronization.
    #[must_use]
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Sets the planned trajectory inherited by subsequent tracks.
    pub fn set_trajectory(&mut self, trajectory: &TempoTrajectory) {
        self.host = trajectory.clone();
    }
}

impl<S, F: TrackFactory<S>> TrackFactory<S> for LinkedFactory<F> {
    type Track = Linked<F::Track>;

    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        let mut track = Linked::new(
            LinkedConfig::builder()
                .inner(self.inner.track(config)?)
                .settings(self.config)
                .trajectory(self.host.clone())
                .build(),
        );
        track.mode = if self.synced {
            SyncMode::On
        } else {
            SyncMode::Off
        };
        Ok(track)
    }
}
