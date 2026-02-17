use tokio::sync::broadcast;

use crate::events::PlayerEvent;
use crate::item::PlayerItem;
use crate::types::SlotId;

pub trait QueuePlayer: Send + Sync + 'static {
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
