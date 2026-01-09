use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use futures::Stream;
use tokio::sync::{broadcast, mpsc};

use super::types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentPayload, SegmentStream,
};
use crate::{
    abr::{AbrConfig, AbrController, AbrDecision, AbrReason, ThroughputSample, Variant},
    playlist::MasterPlaylist,
};

/// ABR-оверлей: владеет ABR контроллером напрямую, отслеживает метрики,
/// принимает решения о переключении и транслирует команды вниз.
pub struct AbrStream<I>
where
    I: SegmentStream,
{
    inner: Pin<Box<I>>,
    inner_cmd: mpsc::Sender<PipelineCommand>,
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,

    // Внутренний ABR контроллер, которым владеем напрямую
    abr_controller: AbrController,

    // Атомарная ссылка на текущий вариант для быстрого доступа
    current_variant: Arc<AtomicUsize>,
}

impl<I> AbrStream<I>
where
    I: SegmentStream,
{
    pub fn new(inner: I, abr_controller: AbrController) -> Self {
        let inner_cmd = inner.command_sender();
        let events = inner.event_sender();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        let initial_variant = abr_controller.current_variant();
        let current_variant = Arc::new(AtomicUsize::new(initial_variant));

        Self {
            inner: Box::pin(inner),
            inner_cmd,
            events,
            cmd_tx,
            cmd_rx,
            abr_controller,
            current_variant,
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
                    let from = self.abr_controller.current_variant();
                    self.abr_controller.set_current_variant(variant_index);
                    self.current_variant.store(variant_index, Ordering::Relaxed);

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

    /// Добавить throughput sample для ABR логики
    pub fn add_throughput_sample(&mut self, sample: ThroughputSample) {
        self.abr_controller.push_throughput_sample(sample);
    }

    /// Принять ABR решение на основе текущих метрик
    pub fn decide_abr(&self, variants: &[Variant], buffer_level_secs: f64) -> AbrDecision {
        self.abr_controller
            .decide(variants, buffer_level_secs, std::time::Instant::now())
    }

    /// Получить текущий выбранный вариант
    pub fn current_variant(&self) -> usize {
        self.current_variant.load(Ordering::Relaxed)
    }

    /// Получить mutable ссылку на внутренний ABR контроллер
    pub fn abr_controller_mut(&mut self) -> &mut AbrController {
        &mut self.abr_controller
    }

    /// Получить immutable ссылку на внутренний ABR контроллер
    pub fn abr_controller(&self) -> &AbrController {
        &self.abr_controller
    }

    /// Принять решение для мастер-плейлиста
    pub fn decide_for_master(
        &self,
        master: &MasterPlaylist,
        variants: &[Variant],
        buffer_level_secs: f64,
    ) -> AbrDecision {
        self.abr_controller.decide_for_master(
            master,
            variants,
            buffer_level_secs,
            std::time::Instant::now(),
        )
    }
}

impl<I> Stream for AbrStream<I>
where
    I: SegmentStream,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Err(err) = self.handle_commands() {
            return Poll::Ready(Some(Err(err)));
        }

        self.inner.as_mut().poll_next(cx)
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
