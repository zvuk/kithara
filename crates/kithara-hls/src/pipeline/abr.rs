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
use crate::abr::AbrController;

/// ABR-оверлей: отслеживает текущий вариант, транслирует команды вниз,
/// публикует события о смене варианта.
pub struct AbrStream<I>
where
    I: SegmentStream,
{
    inner: Pin<Box<I>>,
    inner_cmd: mpsc::Sender<PipelineCommand>,
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    cancel: CancellationToken,
    current_variant: usize,
}

impl<I> AbrStream<I>
where
    I: SegmentStream,
{
    pub fn new(
        inner: I,
        _abr: std::sync::Arc<tokio::sync::Mutex<AbrController>>,
        cancel: CancellationToken,
        initial_variant: usize,
    ) -> Self {
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
            current_variant: initial_variant,
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
                    let from = self.current_variant;
                    self.current_variant = variant_index;

                    // Обновляем ABR контроллер асинхронно
                    // Мы не можем блокировать здесь, поэтому сохраняем команду
                    // для асинхронной обработки. Вместо этого просто обновляем
                    // локальное состояние и передаем команду вниз.
                    // ABR контроллер будет обновлен при следующем вызове
                    // через отдельную задачу или при следующем взаимодействии.
                    // Для простоты пока просто обновляем локальное состояние.

                    // Отправляем событие о выборе варианта
                    let _ = self.events.send(PipelineEvent::VariantSelected {
                        from,
                        to: variant_index,
                    });

                    // Передаем команду вниз по пайплайну
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

    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
