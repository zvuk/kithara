use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::types::{PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentPayload};

/// Prefetch stage: буферизует элементы из нижнего слоя до `capacity`,
/// заполняет буферы последовательно (один за другим), а не параллельно.
/// Пробрасывает команды вниз, отправляет событие `Prefetched`, реагирует на cancellation.
pub struct PrefetchStream<I>
where
    I: PipelineStream,
{
    inner: Pin<Box<I>>,
    events: broadcast::Sender<PipelineEvent>,
    cancel: CancellationToken,

    // Основной буфер для выдачи данных потребителю
    output_buffer: VecDeque<PipelineResult<SegmentPayload>>,

    // Общая емкость префетча (сколько сегментов хранить заранее)
    capacity: usize,

    // Флаг окончания потока
    ended: bool,
}

impl<I> PrefetchStream<I>
where
    I: PipelineStream,
{
    pub fn new(inner: I, capacity: usize, cancel: CancellationToken) -> Self {
        let events = inner.event_sender();

        Self {
            inner: Box::pin(inner),
            events,
            cancel,
            output_buffer: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            ended: false,
        }
    }

    /// Заполняет буфер до емкости или пока нет новых данных
    fn fill_buffer(&mut self, cx: &mut Context<'_>) {
        while self.output_buffer.len() < self.capacity && !self.ended {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(res)) => {
                    if let Ok(ref payload) = res {
                        let _ = self.events.send(PipelineEvent::Prefetched {
                            variant: payload.meta.variant,
                            segment_index: payload.meta.segment_index,
                        });
                    }
                    self.output_buffer.push_back(res);
                }
                Poll::Ready(None) => {
                    self.ended = true;
                    break;
                }
                Poll::Pending => break,
            }
        }
    }
}

impl<I> Stream for PrefetchStream<I>
where
    I: PipelineStream,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.cancel.is_cancelled() {
            return Poll::Ready(Some(Err(PipelineError::Aborted)));
        }

        // Всегда пытаемся подзаполнить буфер перед выдачей
        self.fill_buffer(cx);

        if let Some(item) = self.output_buffer.pop_front() {
            if self.output_buffer.len() < self.capacity && !self.ended {
                self.fill_buffer(cx);
            }
            return Poll::Ready(Some(item));
        }

        if self.ended {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

impl<I> PipelineStream for PrefetchStream<I>
where
    I: PipelineStream,
{
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
