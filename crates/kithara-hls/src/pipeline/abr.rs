use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentPayload, SegmentStream,
};
use crate::abr::AbrController;

/// ABR-оверлей: обновляет AbrController при ForceVariant, транслирует команды вниз,
/// публикует свои события в общий канал, напрямую опрашивает вложенный слой.
pub struct AbrStream<I>
where
    I: SegmentStream,
{
    inner: Pin<Box<I>>,
    inner_cmd: mpsc::Sender<PipelineCommand>,
    events: tokio::sync::broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    abr: Arc<Mutex<AbrController>>,
    cancel: CancellationToken,
}

impl<I> AbrStream<I>
where
    I: SegmentStream,
{
    pub fn new(inner: I, abr: Arc<Mutex<AbrController>>, cancel: CancellationToken) -> Self {
        let inner_cmd = inner.command_sender();
        let events = inner.event_sender();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        Self {
            inner: Box::pin(inner),
            inner_cmd,
            events,
            cmd_tx,
            cmd_rx,
            abr,
            cancel,
        }
    }

    fn handle_commands(&mut self) -> Result<bool, PipelineError> {
        let mut changed = false;

        while let Ok(cmd) = self.cmd_rx.try_recv() {
            changed = true;
            match cmd {
                PipelineCommand::Seek { segment_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::Seek { segment_index });
                }
                PipelineCommand::ForceVariant { variant_index } => {
                    let from = {
                        // Fast path: read current without holding lock across await.
                        let guard = futures::executor::block_on(self.abr.lock());
                        guard.current_variant()
                    };
                    {
                        let mut guard = futures::executor::block_on(self.abr.lock());
                        guard.set_current_variant(variant_index);
                    }
                    let _ = self.events.send(PipelineEvent::VariantSelected {
                        from,
                        to: variant_index,
                    });
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::ForceVariant { variant_index });
                }
                PipelineCommand::Shutdown => {
                    let _ = self.inner_cmd.try_send(PipelineCommand::Shutdown);
                    return Err(PipelineError::Aborted);
                }
            }
        }

        Ok(changed)
    }
}

impl<I> Stream for AbrStream<I>
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

        this.inner.as_mut().poll_next(cx)
    }
}

impl<I> SegmentStream for AbrStream<I>
where
    I: SegmentStream,
{
    fn command_sender(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn event_sender(&self) -> tokio::sync::broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
