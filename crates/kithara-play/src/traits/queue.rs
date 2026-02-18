use kithara_platform::{MaybeSend, MaybeSync};
use tokio::sync::broadcast;

use crate::{events::PlayerEvent, traits::item::PlayerItem, types::SlotId};

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = QueuePlayerMock, type Item = unimock::Unimock;)
)]
pub trait QueuePlayer: MaybeSend + MaybeSync + 'static {
    type Item: PlayerItem;

    fn items(&self) -> Vec<&Self::Item>;

    fn advance_to_next_item(&self);

    fn can_insert(&self, item: &Self::Item, after: Option<&Self::Item>) -> bool;

    fn insert(&self, item: Self::Item, after: Option<&Self::Item>);

    fn remove(&self, item: &Self::Item);

    fn remove_all_items(&self);

    // -- inherited from Player semantics --

    fn play(&self);

    fn pause(&self);

    fn current_item(&self) -> Option<&Self::Item>;

    fn slot_id(&self) -> Option<SlotId>;

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use unimock::Unimock;

    fn assert_queue_item_unimock<T: QueuePlayer<Item = Unimock>>() {}

    #[test]
    fn queue_player_mock_api_is_generated() {
        let _ = QueuePlayerMock::items;
    }

    #[test]
    fn queue_player_unimock_item_is_unimock() {
        assert_queue_item_unimock::<Unimock>();
    }
}
