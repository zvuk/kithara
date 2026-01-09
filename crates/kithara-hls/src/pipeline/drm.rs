use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{Future, Stream};
use tokio::sync::{broadcast, mpsc};
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
    ) -> Pin<Box<dyn Future<Output = Result<SegmentPayload, HlsError>> + Send>>;
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
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    decrypter: Option<Arc<D>>,
    cancel: CancellationToken,
    pending_decrypt: Option<Pin<Box<dyn Future<Output = Result<SegmentPayload, HlsError>> + Send>>>,
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
            pending_decrypt: None,
        }
    }

    fn handle_commands(&mut self) -> Result<(), PipelineError> {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                PipelineCommand::Seek { segment_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::Seek { segment_index });
                    // Сбрасываем ожидающую расшифровку, так как seek меняет позицию
                    self.pending_decrypt = None;
                }
                PipelineCommand::ForceVariant { variant_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::ForceVariant { variant_index });
                    // Сбрасываем ожидающую расшифровку, так как смена варианта меняет данные
                    self.pending_decrypt = None;
                }
            }
        }
        Ok(())
    }
}

impl<I, D> Stream for DrmStream<I, D>
where
    I: SegmentStream,
    D: Decrypter,
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
        if let Some(mut fut) = self.pending_decrypt.take() {
            match fut.as_mut().poll(cx) {
                Poll::Ready(Ok(payload)) => {
                    // Отправляем событие о расшифровке
                    let _ = self.events.send(PipelineEvent::Decrypted {
                        variant: payload.meta.variant,
                        segment_index: payload.meta.segment_index,
                    });
                    return Poll::Ready(Some(Ok(payload)));
                }
                Poll::Ready(Err(err)) => {
                    return Poll::Ready(Some(Err(PipelineError::Hls(err))));
                }
                Poll::Pending => {
                    self.pending_decrypt = Some(fut);
                    return Poll::Pending;
                }
            }
        }

        // Опрашиваем внутренний поток
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(payload))) => {
                if let Some(decrypter) = self.decrypter.as_ref() {
                    // Начинаем расшифровку
                    self.pending_decrypt = Some(decrypter.decrypt(payload));
                    // Рекурсивно опрашиваем себя для обработки расшифровки
                    self.poll_next(cx)
                } else {
                    // Нет decryptor, возвращаем как есть
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

    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
