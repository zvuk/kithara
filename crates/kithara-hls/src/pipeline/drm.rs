use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentPayload, SegmentStream,
};
use crate::HlsError;

/// Трейт расшифровки сегмента: принимает payload и возвращает новый payload с расшифрованными байтами.
pub trait Decrypter: Send + Sync + 'static {
    fn decrypt(
        &self,
        payload: SegmentPayload,
    ) -> Pin<Box<dyn Future<Output = Result<SegmentPayload, HlsError>> + Send + 'static>>;
}

struct PendingDecrypt {
    fut: Pin<Box<dyn Future<Output = Result<SegmentPayload, HlsError>> + Send>>,
}

/// DRM-оверлей: расшифровывает сегменты (если задан decryptor), транслирует команды вниз,
/// публикует событие Decrypted, реагирует на CancellationToken.
pub struct DrmStream<I, D>
where
    I: SegmentStream,
    D: Decrypter,
{
    inner: Pin<Box<I>>,
    inner_cmd: mpsc::Sender<PipelineCommand>,
    events: tokio::sync::broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    decrypter: Option<Arc<D>>,
    cancel: CancellationToken,
    pending: Option<PendingDecrypt>,
}

impl<I, D> DrmStream<I, D>
where
    I: SegmentStream,
    D: Decrypter,
{
    pub fn new(inner: I, decrypter: Option<Arc<D>>, cancel: CancellationToken) -> Self {
        let inner_cmd = inner.command_sender();
        let events = inner.event_sender();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        Self {
            inner: Box::pin(inner),
            inner_cmd,
            events,
            cmd_tx,
            cmd_rx,
            decrypter,
            cancel,
            pending: None,
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
                PipelineCommand::Shutdown => {
                    let _ = self.inner_cmd.try_send(PipelineCommand::Shutdown);
                    return Err(PipelineError::Aborted);
                }
            }
        }
        Ok(())
    }

    fn poll_pending(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<PipelineResult<SegmentPayload>>> {
        let Some(pending) = self.pending.as_mut() else {
            return Poll::Pending;
        };

        match pending.fut.as_mut().poll(cx) {
            Poll::Ready(res) => {
                let payload = match res {
                    Ok(payload) => {
                        let _ = self.events.send(PipelineEvent::Decrypted {
                            variant: payload.meta.variant,
                            segment_index: payload.meta.segment_index,
                        });
                        Ok(payload)
                    }
                    Err(err) => Err(PipelineError::Hls(err)),
                };
                self.pending = None;
                Poll::Ready(Some(payload))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<I, D> Stream for DrmStream<I, D>
where
    I: SegmentStream,
    D: Decrypter,
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

        if let Poll::Ready(res) = this.poll_pending(cx) {
            return Poll::Ready(res);
        }

        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(payload))) => {
                if let Some(dec) = this.decrypter.as_ref() {
                    let fut = dec.decrypt(payload);
                    this.pending = Some(PendingDecrypt { fut });
                    this.poll_pending(cx)
                } else {
                    Poll::Ready(Some(Ok(payload)))
                }
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<I, D> SegmentStream for DrmStream<I, D>
where
    I: SegmentStream,
    D: Decrypter,
{
    fn command_sender(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn event_sender(&self) -> tokio::sync::broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
