use kithara_bufpool::HasPool;
use kithara_command::Seq;
use kithara_play::{
    Bound, DeckControl, Outbox, PlayError, Player, Settled, TrackFactory, TrackReceipt,
};
use kithara_signal::SessionFrame;

use super::super::{Queue, QueueCommand, QueueControl, QueueSnapshot};

impl<S, F> DeckControl for Queue<S, F>
where
    S: HasPool<u8> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    type Control = QueueControl<S>;

    fn control(&self) -> Self::Control {
        Self::control(self)
    }
}

impl<S, F> Player<S> for Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    type Command = QueueCommand<S>;
    type Snapshot = QueueSnapshot<S>;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        self.current_track().and_then(|track| track.entry(bound))
    }

    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let output = out.pass().map(|pass| *pass.output);
        self.apply_with_output(command, output.as_ref(), out)
    }

    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled {
        let output = out.pass().map(|pass| *pass.output);
        self.settle_with_output(receipt, output.as_ref(), out)
    }

    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, S>) {
        let output = out.pass().map(|pass| *pass.output);
        self.tick_with_output(now, output.as_ref(), out);
    }

    fn snapshot(&self) -> Self::Snapshot {
        self.queue_snapshot()
    }
}
