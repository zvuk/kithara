use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use super::types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentPayload, SegmentStream,
};

/// Prefetch stage: буферизует элементы из нижнего слоя до `capacity`,
/// пробрасывает команды вниз, отправляет событие `Prefetched`, реагирует на cancellation.
pub struct PrefetchStream<I>
where
    I: SegmentStream,
{
    inner: Pin<Box<I>>,
    inner_cmd: mpsc::Sender<PipelineCommand>,
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    cancel: CancellationToken,
    buf: VecDeque<PipelineResult<SegmentPayload>>,
    capacity: usize,
    ended: bool,
}

impl<I> PrefetchStream<I>
where
    I: SegmentStream,
{
    pub fn new(inner: I, capacity: usize, cancel: CancellationToken) -> Self {
        let inner_cmd = inner.command_sender();
        let events = inner.event_sender();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        Self {
            inner: Box::pin(inner),
            inner_cmd,
            events,
            cmd_tx,
            cmd_rx,
            cancel,
            buf: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            ended: false,
        }
    }

    fn handle_commands(&mut self) -> Result<(), PipelineError> {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                PipelineCommand::Seek { segment_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::Seek { segment_index });
                    self.buf.clear();
                    self.ended = false;
                }
                PipelineCommand::ForceVariant { variant_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::ForceVariant { variant_index });
                    self.buf.clear();
                    self.ended = false;
                }
                PipelineCommand::Shutdown => {
                    let _ = self.inner_cmd.try_send(PipelineCommand::Shutdown);
                    return Err(PipelineError::Aborted);
                }
            }
        }
        Ok(())
    }

    fn fill_buffer(&mut self, cx: &mut Context<'_>) {
        while self.buf.len() < self.capacity && !self.ended {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(res)) => {
                    if let Ok(ref payload) = res {
                        let _ = self.events.send(PipelineEvent::Prefetched {
                            variant: payload.meta.variant,
                            segment_index: payload.meta.segment_index,
                        });
                    }
                    self.buf.push_back(res);
                }
                Poll::Ready(None) => {
                    self.ended = true;
                }
                Poll::Pending => break,
            }
        }
    }
}

impl<I> Stream for PrefetchStream<I>
where
    I: SegmentStream,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.cancel.is_cancelled() {
            let _ = this.inner_cmd.try_send(PipelineCommand::Shutdown);
            return Poll::Ready(Some(Err(PipelineError::Aborted)));
        }

        if let Err(err) = this.handle_commands() {
            return Poll::Ready(Some(Err(err)));
        }

        this.fill_buffer(cx);

        if let Some(item) = this.buf.pop_front() {
            return Poll::Ready(Some(item));
        }

        if this.ended {
            return Poll::Ready(None);
        }

        Poll::Pending
    }
}

impl<I> SegmentStream for PrefetchStream<I>
where
    I: SegmentStream,
{
    fn command_sender(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
