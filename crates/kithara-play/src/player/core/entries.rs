use std::num::NonZeroU32;

use kithara_platform::sync::Arc;
use kithara_test_macros as kithara;
use kithara_warp::{
    AlignmentSource, MapAxis, PresentationFrontier, ReconcileCause, SessionFrame, SyncAdmission,
    SyncMode, SyncRejected, WarpMap, WarpPlan,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::PlayerImpl;
use crate::{
    api::TrackId,
    bridge::{PreparedLaunchIdentity, ScheduledSeekDisposition},
    error::PlayError,
    player::{
        core::grids::{native_frame, source_cue_beat, source_duration},
        protocol::PlayerMember,
    },
    session::SessionError,
    sync::EntryRefusal,
};

impl<S> PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    /// Reconciles the initial host-synced track and prepares queued launches.
    ///
    /// The audible track's own stream says where the crossfade begins, and the
    /// deck carries that frame onto the owner axis as the deadline a waiting
    /// track must enter by. A track that already holds a prepared map keeps
    /// it: the deadline does not move while the same track stays audible, and
    /// a second map would carry a warp map the launch the slot holds does
    /// not.
    pub(crate) fn prepare_sync_launches(
        &mut self,
        output_now: SessionFrame,
    ) -> Result<(), PlayError> {
        let response_budget = i64::try_from(self.runtime.core.response_budget_frames.get())
            .map_err(|_| PlayError::from(SessionError::TransportFrameExhausted))?;
        let activation_floor = i64::from(output_now)
            .checked_add(response_budget)
            .map(SessionFrame::new)
            .ok_or_else(|| PlayError::from(SessionError::TransportFrameExhausted))?;

        if let Some(item) = self.runtime.core.items.current_item_id()
            && self.sync.mode() == SyncMode::HostSync
            && self.runtime.presentation_frontier_for(item, None).is_none()
            && self
                .runtime
                .core
                .items
                .track_grid(item)
                .is_some_and(|grid| self.sync.prepared().get(grid.id).is_none())
        {
            let source = AlignmentSource::Prepared(
                PresentationFrontier::builder()
                    .output(activation_floor)
                    .source(0)
                    .build(),
            );
            self.reconcile_item_grid(
                item,
                ReconcileCause::GridAvailable,
                Some(source),
                true,
                None,
            )
            .map_err(|rejected| {
                let (error, _) = rejected.into();
                PlayError::from(error)
            })?;
        }

        self.prepare_queued_entries();
        Ok(())
    }

    fn prepare_queued_entries(&mut self) {
        let Some(current) = self.runtime.core.items.current_item_id() else {
            return;
        };
        let Some(audible) = self.runtime.core.items.track_grid(current) else {
            return;
        };
        if !self.has_queued_grid(current) {
            return;
        }
        let Some(snapshot) = self.runtime.playback_snapshot() else {
            return;
        };
        let axis = audible.snapshot.axis();
        let output_rate = self.runtime.core.engine.output_sample_rate();
        let observed = self.runtime.presentation_frontier();
        let frontier = observed.on_axis(axis, output_rate);
        let source = AlignmentSource::Audible {
            presentation: frontier,
            preparation_source: native_frame(
                snapshot.preparation_source(
                    observed.source(),
                    self.runtime.core.response_budget_frames,
                ),
                axis,
                output_rate,
            ),
            playback_rate: kithara_warp::RateTarget::default().with_speed(snapshot.rate),
        };
        let Some(fade_source) = crossfade_source(
            snapshot.duration(),
            f64::from(self.runtime.crossfade_duration()),
            axis,
            output_rate,
        ) else {
            return;
        };
        let Some(window) = self.sync.entry_window(audible.id, fade_source, source) else {
            return;
        };
        for index in 0..self.runtime.core.items.item_count() {
            let Some(item) = self.runtime.core.items.item_id(index) else {
                continue;
            };
            if item == current {
                continue;
            }
            let Some(grid) = self.runtime.core.items.track_grid(item) else {
                continue;
            };
            self.sync.discard_stale_entry(grid.id, window);
            let cue = self
                .runtime
                .core
                .items
                .initial_source_cue(item)
                .and_then(|cue| source_cue_beat(&grid.snapshot, cue));
            if self.sync.prepared().get(grid.id).is_none() {
                match self.sync.prepare_entry(grid.id, window, cue) {
                    Ok(_) => {}
                    Err(EntryRefusal::Geometry(required)) => {
                        tracing::debug!(?required, member = %grid.id, "queued track has no entry geometry");
                    }
                    Err(EntryRefusal::Group(error)) => {
                        tracing::debug!(%error, member = %grid.id, "queued track refused its entry");
                    }
                }
            }
            if let Some(prepared) = self.sync.prepared().get(grid.id) {
                self.deliver_prepared_map(item, &prepared, true);
            }
        }
    }

    /// Carries one prepared map of `item` into the renderer.
    ///
    /// The track's plan holds the map for the decoder, and the
    /// scheduled seek carries the source frame the map activates on. A track
    /// the deck has yet to play enters as a launch, so the renderer starts it
    /// on its activation instead of seeking an audible stream.
    ///
    /// The queued launch remains unarmed until the selected handover commits;
    /// preparation must not make every waiting track eligible to start.
    pub(super) fn deliver_prepared_map(
        &self,
        item: TrackId,
        prepared: &crate::sync::prepare::PreparedSync,
        launch: bool,
    ) {
        self.install_prepared_plan(item, prepared);
        kithara::probe_event!(
            prepared_map_delivered,
            item = item.as_u64(),
            launch = u64::from(launch),
            warp_map_revision = u64::from(prepared.warp_map),
            activation_source = prepared.source,
            activation_output = i64::from(prepared.activation)
        );
        let Some(slot) = self.runtime.slot() else {
            return;
        };
        let disposition = if launch {
            ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
                activation: prepared.activation,
                warp_map: prepared.warp_map,
            })
        } else {
            ScheduledSeekDisposition::SeekOnly {
                activation: prepared.activation,
            }
        };
        if let Err(error) = self.runtime.core.engine.schedule_track_seek(
            slot,
            item,
            source_duration(
                prepared.source,
                self.runtime.core.engine.output_sample_rate(),
            ),
            disposition,
        ) {
            warn!(%error, %item, "prepared map has no scheduled seek");
        }
    }

    /// Installs the plan `prepared` activated, measured against its projection.
    ///
    /// The projection was frozen when the map was prepared, against the owner
    /// grid the map aligns to, so the renderer cannot measure spans against a
    /// grid the activation was never computed from.
    pub(super) fn install_prepared_plan(
        &self,
        item: TrackId,
        prepared: &crate::sync::prepare::PreparedSync,
    ) {
        let activation = WarpMap::identity(prepared.warp_map).reanchor(
            prepared.source,
            prepared.activation,
            prepared.activation_beat,
        );
        self.runtime.core.items.set_track_plan(
            item,
            Some(Arc::new(
                WarpPlan::new(prepared.projection.clone()).with_activation(activation),
            )),
        );
    }

    pub(crate) fn reconcile_current_grid(
        &mut self,
        cause: ReconcileCause,
        source: Option<AlignmentSource>,
        prepared_launch: bool,
    ) -> Result<Option<SyncAdmission>, SyncRejected<PlayerMember>> {
        let Some(item) = self.runtime.core.items.current_item_id() else {
            return Ok(None);
        };
        let source_cue = prepared_launch
            .then(|| {
                self.runtime.core.items.track_grid(item).and_then(|grid| {
                    self.runtime
                        .core
                        .items
                        .initial_source_cue(item)
                        .and_then(|cue| source_cue_beat(&grid.snapshot, cue))
                })
            })
            .flatten();
        self.reconcile_item_grid(item, cause, source, prepared_launch, source_cue)
    }

    /// Whether any queued track other than the audible one carries a grid.
    fn has_queued_grid(&self, current: TrackId) -> bool {
        (0..self.runtime.core.items.item_count()).any(|index| {
            self.runtime.core.items.item_id(index).is_some_and(|item| {
                item != current && self.runtime.core.items.track_grid(item).is_some()
            })
        })
    }
}

/// The frame of the audible stream where its crossfade begins.
///
/// Both durations are media seconds, so carrying them onto the stream's own
/// axis gives its source frame regardless of the rate the deck plays at.
fn crossfade_source(
    duration_seconds: f64,
    fade_seconds: f64,
    axis: MapAxis,
    output_rate: NonZeroU32,
) -> Option<u64> {
    let audible = duration_seconds - fade_seconds.max(0.0);
    if !audible.is_finite() || audible <= 0.0 {
        return None;
    }
    let output_frames = (audible * f64::from(output_rate.get())).round().to_u64()?;
    Some(native_frame(output_frames, axis, output_rate))
}
