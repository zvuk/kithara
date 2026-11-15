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
    F: TrackFactory,
{
    type Control = QueueControl<S>;

    fn control(&self) -> Self::Control {
        Self::control(self)
    }
}

impl<S, F> Player for Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory,
{
    type Command = QueueCommand<S>;
    type Snapshot = QueueSnapshot<S>;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        self.current_track().and_then(|track| track.entry(bound))
    }

    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        let output = out.pass().map(|pass| *pass.output);
        self.apply_with_output(command, output.as_ref(), out)
    }

    fn settle(&mut self, receipt: TrackReceipt<'_>, out: &mut Outbox<'_>) -> Settled {
        let output = out.pass().map(|pass| *pass.output);
        self.settle_with_output(receipt, output.as_ref(), out)
    }

    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_>) {
        let output = out.pass().map(|pass| *pass.output);
        self.tick_with_output(now, output.as_ref(), out);
    }

    fn snapshot(&self) -> Self::Snapshot {
        QueueSnapshot::clone(&self.view.read())
    }
}
