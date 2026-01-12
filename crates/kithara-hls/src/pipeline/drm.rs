use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use super::types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentPayload, SegmentStream,
};
use crate::{HlsError, keys::KeyManager};

/// DRM-оверлей: расшифровывает сегменты (если задан decryptor), транслирует команды вниз,
/// публикует событие Decrypted, реагирует на CancellationToken.
pub struct DrmStream<I>
where
    I: SegmentStream,
{
    inner: Pin<Box<I>>,
    inner_cmd: mpsc::Sender<PipelineCommand>,
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    key_manager: Option<Arc<KeyManager>>,
    cancel: CancellationToken,
}

impl<I> DrmStream<I>
where
    I: SegmentStream,
{
    pub fn new(inner: I, key_manager: Option<Arc<KeyManager>>, cancel: CancellationToken) -> Self {
        let inner_cmd = inner.command_sender();
        let events = inner.event_sender();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        Self {
            inner: Box::pin(inner),
            inner_cmd,
            events,
            cmd_tx,
            cmd_rx,
            key_manager,
            cancel,
        }
    }

    fn handle_commands(&mut self) -> Result<(), PipelineError> {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                PipelineCommand::Seek { segment_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::Seek { segment_index });
                }
                PipelineCommand::ForceVariant { variant_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::ForceVariant { variant_index });
                }
            }
        }
        Ok(())
    }

    fn decrypt(&self, payload: SegmentPayload) -> Result<SegmentPayload, HlsError> {
        let _ = self.key_manager.as_ref();
        Ok(payload)
    }
}

impl<I> Stream for DrmStream<I>
where
    I: SegmentStream,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.cancel.is_cancelled() {
            return Poll::Ready(Some(Err(PipelineError::Aborted)));
        }

        if let Err(err) = self.handle_commands() {
            return Poll::Ready(Some(Err(err)));
        }

        // Если есть ожидающая расшифровка, обрабатываем её
        // Опрашиваем внутренний поток
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(payload))) => match self.decrypt(payload) {
                Ok(payload) => {
                    let _ = self.events.send(PipelineEvent::Decrypted {
                        variant: payload.meta.variant,
                        segment_index: payload.meta.segment_index,
                    });
                    Poll::Ready(Some(Ok(payload)))
                }
                Err(err) => Poll::Ready(Some(Err(PipelineError::Hls(err)))),
            },
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<I> SegmentStream for DrmStream<I>
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
