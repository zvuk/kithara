use kithara_play::{
    PlayError, Player, PlayerCollector, PlayerComponent, PlayerComponentBox, SeekOutcome, StartAt,
};

use super::Queue;

impl Player for Queue {
    fn pause(&self) -> Result<(), PlayError> {
        Self::pause(self);
        Ok(())
    }

    delegate::delegate! {
        to self {
            #[call(duration_seconds)]
            fn duration_seconds(&self) -> Option<f64>;
            #[call(seek_player)]
            fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError>;
            #[call(start_at_player)]
            fn start_at(&self, start: StartAt) -> Result<(), PlayError>;
        }
    }
}

impl PlayerComponent for Queue {
    fn collect_players(&self, collector: &mut PlayerCollector<'_>) {
        collector.push(self.player.clone());
    }
}

impl From<Queue> for PlayerComponentBox {
    fn from(queue: Queue) -> Self {
        let component: Box<dyn PlayerComponent> = Box::new(queue);
        Self::from(component)
    }
}

#[cfg(test)]
mod tests {
    use kithara_play::MultiPlayer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::QueueConfig;

    #[kithara::test]
    fn queue_registers_as_a_routed_player_component() {
        let players = MultiPlayer::default();
        let member = players
            .register(Queue::new(QueueConfig::default()))
            .expect("queue registers");

        member.pause().expect("queue pauses through routed member");
    }
}
