use firewheel::FirewheelContext;
use kithara_config::ConfigOwner;
use kithara_warp::{BeatGridState, MapAxis};

use super::{
    commit::{SessionGridGeneration, TransportObservation},
    event::TransportEvent,
    process::converge_transport_restart,
};
use crate::session::{SessionError, state::SessionState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteRestartStatus {
    Pending,
    Ready,
}

/// Publishes the route boundary on the host grid at the rate the settings ask
/// the next stream for.
pub(crate) fn prepare_route_restart<T, S>(
    state: &mut SessionState<T, S>,
) -> Result<RouteRestartStatus, SessionError> {
    state.iteration_clock = None;
    let was_running = state
        .ctx
        .as_ref()
        .ok_or(SessionError::NoContext)?
        .is_active();
    let current = state.root.grid();
    let MapAxis::Session(axis) = current.axis() else {
        return Err(SessionError::Graph(
            "session host published a non-session grid axis".to_owned(),
        ));
    };
    let target = if let Some(target) = state.reserved_session_grid {
        let target_stamp = target
            .stamp()
            .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
        if was_running
            || !matches!(current.state(), BeatGridState::Unavailable(_))
            || current.stamp() != target_stamp
            || axis.epoch() != target.epoch()
        {
            return Err(SessionError::Graph(
                "reserved route boundary does not match the stopped session".to_owned(),
            ));
        }
        target
    } else {
        let observed = state
            .transport_observation
            .as_mut()
            .ok_or_else(|| {
                SessionError::Graph("session transport observation is missing".to_owned())
            })?
            .read()
            .session_grid();
        if observed.epoch() < axis.epoch() {
            return Err(SessionError::Graph(
                "session transport generation trails the published host grid".to_owned(),
            ));
        }
        let mut target = observed;
        if was_running || observed.epoch() == axis.epoch() {
            target
                .advance_restart()
                .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
        }
        let stamp = target
            .stamp()
            .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
        let sample_rate = state.settings.config().sample_rate();
        state
            .root
            .publish_unavailable(stamp, sample_rate, target.epoch());
        state.publish_root();
        state.reserved_session_grid = Some(target);
        target
    };

    if was_running {
        state
            .ctx
            .as_mut()
            .ok_or(SessionError::NoContext)?
            .request_deactivate();
        state.stream = None;
    }
    state.publish_root();
    finish_route_restart(state, target)
}

/// Once the stopped stream's processor is back, converges its grid while
/// settling applied root changes before seeding its settings from the owner.
fn finish_route_restart<T, S>(
    state: &mut SessionState<T, S>,
    target: SessionGridGeneration,
) -> Result<RouteRestartStatus, SessionError> {
    if state
        .ctx
        .as_ref()
        .ok_or(SessionError::NoContext)?
        .proc_store()
        .is_none()
    {
        return Ok(RouteRestartStatus::Pending);
    }
    crate::session::queue::settle_root_receipts(state);
    let settings = *state.settings.config();
    let Some(store) = state
        .ctx
        .as_mut()
        .and_then(FirewheelContext::proc_store_mut)
    else {
        return Ok(RouteRestartStatus::Pending);
    };
    let actual = converge_transport_restart(store, settings, target)
        .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
    let promoted = target
        .promote(actual)
        .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
    if promoted != target {
        let MapAxis::Session(published_axis) = state.root.grid().axis() else {
            return Err(SessionError::Graph(
                "session host published a non-session grid axis".to_owned(),
            ));
        };
        let stamp = promoted
            .stamp()
            .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
        state
            .root
            .publish_unavailable(stamp, published_axis.sample_rate(), promoted.epoch());
        state.publish_root();
        state.reserved_session_grid = Some(promoted);
    }
    let observed = state
        .transport_observation
        .as_mut()
        .ok_or_else(|| SessionError::Graph("session transport observation is missing".to_owned()))?
        .read()
        .session_grid();
    if observed != promoted {
        return Err(SessionError::Graph(
            "session transport did not converge to the reserved route boundary".to_owned(),
        ));
    }
    Ok(RouteRestartStatus::Ready)
}

/// Brings the Host grid up to what the render graph has committed: on every
/// session tick and offline block, so the Host grid follows the tempo it
/// clicks with no deck ticking.
///
/// Nothing is committed while no graph runs or a route restart holds the
/// session grid.
pub(crate) fn observe_commits<T, S>(state: &mut SessionState<T, S>) {
    if state.reserved_session_grid.is_some() {
        return;
    }
    let Some(observation) = state.transport_observation.as_mut() else {
        return;
    };
    let observation = *observation.read();
    publish_committed(state, &observation);
}

/// Publishes the committed session grid as the Host grid; idempotent.
fn publish_committed<T, S>(state: &mut SessionState<T, S>, observation: &TransportObservation) {
    if let Some(snapshot) = observation.snapshot()
        && state.root.grid().stamp() != snapshot.session_grid_stamp()
    {
        state.root.publish(snapshot.session_grid());
        state.publish_root();
    }
}

pub(crate) fn publish_transport_event<T, S>(state: &SessionState<T, S>, event: &TransportEvent) {
    for deck in &state.deck_nodes {
        if let Some(bus) = &deck.bus {
            bus.publish(event.clone());
        }
    }
}
