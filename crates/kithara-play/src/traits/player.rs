use kithara_platform::{MaybeSend, MaybeSync, time::Duration, tokio::sync::broadcast};

use crate::{
    error::PlayError,
    events::PlayerEvent,
    time::MediaTime,
    types::{ObserverId, PlayerStatus, SlotId, TimeControlStatus, WaitingReason},
};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

#[kithara::mock(api = PlayerMock, type Item = unimock::Unimock;)]
pub trait Player: MaybeSend + MaybeSync + 'static {
    type Item: crate::traits::item::PlayerItem;

    fn add_boundary_time_observer(
        &self,
        times: Vec<MediaTime>,
        callback: Box<dyn Fn() + Send + 'static>,
    ) -> ObserverId;

    fn add_periodic_time_observer(
        &self,
        interval: Duration,
        callback: Box<dyn Fn(MediaTime) + Send + 'static>,
    ) -> ObserverId;

    fn automatically_waits_to_minimize_stalling(&self) -> bool;

    fn cancel_pending_prerolls(&self);

    fn current_item(&self) -> Option<&Self::Item>;

    fn current_time(&self) -> MediaTime;

    fn error(&self) -> Option<String>;

    fn is_muted(&self) -> bool;

    fn is_network_expensive(&self) -> bool;

    fn pause(&self);

    fn play(&self);

    fn play_immediately_at_rate(&self, rate: f32);

    fn preroll(&self, rate: f32) -> Result<(), PlayError>;

    fn rate(&self) -> f32;

    fn reason_for_waiting_to_play(&self) -> Option<WaitingReason>;

    fn remove_time_observer(&self, id: ObserverId);

    fn replace_current_item(&self, item: Option<Self::Item>);

    fn seek(&self, to: MediaTime);

    fn seek_with_tolerance(
        &self,
        to: MediaTime,
        tolerance_before: MediaTime,
        tolerance_after: MediaTime,
    );

    fn set_automatically_waits_to_minimize_stalling(&self, waits: bool);

    fn set_muted(&self, muted: bool);

    fn set_network_expensive(&self, expensive: bool);

    fn set_rate(&self, rate: f32);

    fn set_rate_with_time(&self, rate: f32, time: MediaTime, at_host_time: MediaTime);

    fn set_volume(&self, volume: f32);

    fn slot_id(&self) -> Option<SlotId>;

    fn status(&self) -> PlayerStatus;

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent>;

    fn time_control_status(&self) -> TimeControlStatus;

    fn volume(&self) -> f32;
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use unimock::Unimock;

    use super::*;

    fn assert_player_item_unimock<T: Player<Item = Unimock>>() {}

    #[kithara::test]
    fn player_mock_api_is_generated() {
        let _ = PlayerMock::status;
    }

    #[kithara::test]
    fn player_unimock_item_is_unimock() {
        assert_player_item_unimock::<Unimock>();
    }
}
