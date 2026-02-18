use std::time::Duration;

use kithara_platform::{MaybeSend, MaybeSync};
use tokio::sync::broadcast;

use crate::{
    error::PlayError,
    events::PlayerEvent,
    time::MediaTime,
    types::{ActionAtItemEnd, ObserverId, PlayerStatus, SlotId, TimeControlStatus, WaitingReason},
};

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = PlayerMock, type Item = unimock::Unimock;)
)]
pub trait Player: MaybeSend + MaybeSync + 'static {
    type Item: crate::traits::item::PlayerItem;

    // -- status --

    fn status(&self) -> PlayerStatus;

    fn error(&self) -> Option<String>;

    fn time_control_status(&self) -> TimeControlStatus;

    fn reason_for_waiting_to_play(&self) -> Option<WaitingReason>;

    // -- current item --

    fn current_item(&self) -> Option<&Self::Item>;

    fn replace_current_item(&self, item: Option<Self::Item>);

    fn action_at_item_end(&self) -> ActionAtItemEnd;

    fn set_action_at_item_end(&self, action: ActionAtItemEnd);

    // -- transport --

    fn play(&self);

    fn pause(&self);

    fn play_immediately_at_rate(&self, rate: f32);

    fn preroll(&self, rate: f32) -> Result<(), PlayError>;

    fn cancel_pending_prerolls(&self);

    // -- timing --

    fn current_time(&self) -> MediaTime;

    fn seek(&self, to: MediaTime);

    fn seek_with_tolerance(
        &self,
        to: MediaTime,
        tolerance_before: MediaTime,
        tolerance_after: MediaTime,
    );

    // -- rate --

    fn rate(&self) -> f32;

    fn set_rate(&self, rate: f32);

    fn set_rate_with_time(&self, rate: f32, time: MediaTime, at_host_time: MediaTime);

    // -- volume --

    fn volume(&self) -> f32;

    fn set_volume(&self, volume: f32);

    fn is_muted(&self) -> bool;

    fn set_muted(&self, muted: bool);

    // -- buffering --

    fn automatically_waits_to_minimize_stalling(&self) -> bool;

    fn set_automatically_waits_to_minimize_stalling(&self, waits: bool);

    // -- network --

    fn is_network_expensive(&self) -> bool;

    fn set_network_expensive(&self, expensive: bool);

    // -- time observation --

    fn add_periodic_time_observer(
        &self,
        interval: Duration,
        callback: Box<dyn Fn(MediaTime) + Send + 'static>,
    ) -> ObserverId;

    fn add_boundary_time_observer(
        &self,
        times: Vec<MediaTime>,
        callback: Box<dyn Fn() + Send + 'static>,
    ) -> ObserverId;

    fn remove_time_observer(&self, id: ObserverId);

    // -- events --

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent>;

    // -- engine integration --

    fn slot_id(&self) -> Option<SlotId>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use unimock::Unimock;

    fn assert_player_item_unimock<T: Player<Item = Unimock>>() {}

    #[test]
    fn player_mock_api_is_generated() {
        let _ = PlayerMock::status;
    }

    #[test]
    fn player_unimock_item_is_unimock() {
        assert_player_item_unimock::<Unimock>();
    }
}
