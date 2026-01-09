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
/// заполняет буферы последовательно (один за другим), а не параллельно.
/// Пробрасывает команды вниз, отправляет событие `Prefetched`, реагирует на cancellation.
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

    // Основной буфер для выдачи данных потребителю
    output_buffer: VecDeque<PipelineResult<SegmentPayload>>,

    // Текущий заполняемый буфер (заполняется последовательно)
    current_filling_buffer: VecDeque<PipelineResult<SegmentPayload>>,

    // Общая емкость префетча (сколько сегментов хранить заранее)
    capacity: usize,

    // Размер текущего заполняемого буфера (сколько уже заполнено в текущем буфере)
    current_buffer_fill: usize,

    // Флаг окончания потока
    ended: bool,

    // Флаг, что мы в процессе заполнения буфера
    is_filling: bool,
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
            output_buffer: VecDeque::with_capacity(capacity.max(1)),
            current_filling_buffer: VecDeque::new(),
            capacity: capacity.max(1),
            current_buffer_fill: 0,
            ended: false,
            is_filling: false,
        }
    }

    fn handle_commands(&mut self) -> Result<(), PipelineError> {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                PipelineCommand::Seek { segment_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::Seek { segment_index });
                    // Сбрасываем все буферы при seek
                    self.output_buffer.clear();
                    self.current_filling_buffer.clear();
                    self.current_buffer_fill = 0;
                    self.ended = false;
                    self.is_filling = false;
                }
                PipelineCommand::ForceVariant { variant_index } => {
                    let _ = self
                        .inner_cmd
                        .try_send(PipelineCommand::ForceVariant { variant_index });
                    // Сбрасываем все буферы при смене варианта
                    self.output_buffer.clear();
                    self.current_filling_buffer.clear();
                    self.current_buffer_fill = 0;
                    self.ended = false;
                    self.is_filling = false;
                }
                PipelineCommand::Shutdown => {
                    let _ = self.inner_cmd.try_send(PipelineCommand::Shutdown);
                    return Err(PipelineError::Aborted);
                }
            }
        }
        Ok(())
    }

    /// Заполняет текущий буфер последовательно до полного заполнения
    fn fill_current_buffer(&mut self, cx: &mut Context<'_>) {
        // Если буфер уже заполнен или поток закончился, не заполняем
        if self.current_buffer_fill >= self.capacity || self.ended {
            return;
        }

        // Заполняем текущий буфер
        while self.current_buffer_fill < self.capacity && !self.ended {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(res)) => {
                    if let Ok(ref payload) = res {
                        // Отправляем событие о префетче
                        let _ = self.events.send(PipelineEvent::Prefetched {
                            variant: payload.meta.variant,
                            segment_index: payload.meta.segment_index,
                        });
                    }
                    self.current_filling_buffer.push_back(res);
                    self.current_buffer_fill += 1;

                    // Если текущий буфер заполнен, перемещаем его в output_buffer
                    if self.current_buffer_fill >= self.capacity {
                        while let Some(item) = self.current_filling_buffer.pop_front() {
                            self.output_buffer.push_back(item);
                        }
                        self.current_buffer_fill = 0;
                        self.is_filling = false;
                        return;
                    }
                }
                Poll::Ready(None) => {
                    // Поток закончился, перемещаем все что есть в output_buffer
                    self.ended = true;
                    while let Some(item) = self.current_filling_buffer.pop_front() {
                        self.output_buffer.push_back(item);
                    }
                    self.current_buffer_fill = 0;
                    self.is_filling = false;
                    return;
                }
                Poll::Pending => {
                    // Больше данных нет сейчас, выходим
                    self.is_filling = true;
                    return;
                }
            }
        }
    }
}

impl<I> Stream for PrefetchStream<I>
where
    I: SegmentStream,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.cancel.is_cancelled() {
            let _ = self.inner_cmd.try_send(PipelineCommand::Shutdown);
            return Poll::Ready(Some(Err(PipelineError::Aborted)));
        }

        if let Err(err) = self.handle_commands() {
            return Poll::Ready(Some(Err(err)));
        }

        // Если в output_buffer есть данные, возвращаем их
        if let Some(item) = self.output_buffer.pop_front() {
            // После извлечения элемента проверяем, нужно ли начать заполнение нового буфера
            if self.output_buffer.len() < self.capacity / 2 && !self.ended && !self.is_filling {
                self.fill_current_buffer(cx);
            }
            return Poll::Ready(Some(item));
        }

        // Если output_buffer пуст, но поток еще не закончился
        if !self.ended {
            // Пытаемся заполнить буфер
            self.fill_current_buffer(cx);

            // Проверяем, появились ли данные в output_buffer после заполнения
            if let Some(item) = self.output_buffer.pop_front() {
                return Poll::Ready(Some(item));
            }

            // Если после заполнения все еще нет данных, но поток не закончился
            if !self.ended {
                return Poll::Pending;
            }
        }

        // Поток закончился и буферы пусты
        Poll::Ready(None)
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
