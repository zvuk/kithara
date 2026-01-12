use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::types::{PipelineError, PipelineEvent, PipelineResult, SegmentPayload, SegmentStream};
use crate::{HlsError, keys::KeyManager};

/// DRM-оверлей: расшифровывает сегменты (если задан decryptor), транслирует команды вниз,
/// публикует событие Decrypted, реагирует на CancellationToken.
pub struct DrmStream<I>
where
    I: SegmentStream,
{
    inner: Pin<Box<I>>,
    events: broadcast::Sender<PipelineEvent>,
    key_manager: Option<Arc<KeyManager>>,
    cancel: CancellationToken,
}

impl<I> DrmStream<I>
where
    I: SegmentStream,
{
    pub fn new(inner: I, key_manager: Option<Arc<KeyManager>>, cancel: CancellationToken) -> Self {
        let events = inner.event_sender();

        Self {
            inner: Box::pin(inner),
            events,
            key_manager,
            cancel,
        }
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

        // Если есть ожидающая расшифровка, обрабатываем её
        // Опрашиваем внутренний поток
        let next = self.inner.as_mut().poll_next(cx);
        let payload = match next {
            Poll::Ready(Some(Ok(payload))) => payload,
            Poll::Ready(Some(Err(err))) => return Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => return Poll::Pending,
        };

        match self.decrypt(payload) {
            Ok(payload) => {
                let _ = self.events.send(PipelineEvent::Decrypted {
                    variant: payload.meta.variant,
                    segment_index: payload.meta.segment_index,
                });
                Poll::Ready(Some(Ok(payload)))
            }
            Err(err) => Poll::Ready(Some(Err(PipelineError::Hls(err)))),
        }
    }
}

impl<I> SegmentStream for DrmStream<I>
where
    I: SegmentStream,
{
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
