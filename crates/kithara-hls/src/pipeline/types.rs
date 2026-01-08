use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use url::Url;

use crate::HlsError;

/// Команды, которыми управляем всей цепочкой.
#[derive(Debug, Clone)]
pub enum PipelineCommand {
    /// Перейти к указанному индексу сегмента.
    Seek { segment_index: usize },
    /// Принудительно выбрать вариант (отключает ABR до следующей команды).
    ForceVariant { variant_index: usize },
    /// Остановить работу пайплайна.
    Shutdown,
}

/// Единый тип событий от всех слоёв.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// ABR принял решение переключить вариант (ещё не применён).
    VariantSelected { from: usize, to: usize },
    /// Вариант применён (базовый слой начал выдавать новый вариант).
    VariantApplied { from: usize, to: usize },
    /// Сегмент готов к выдаче из текущего слоя.
    SegmentReady {
        variant: usize,
        segment_index: usize,
    },
    /// Сегмент успешно расшифрован.
    Decrypted {
        variant: usize,
        segment_index: usize,
    },
    /// Сегмент помещён в буфер префетча.
    Prefetched {
        variant: usize,
        segment_index: usize,
    },
}

/// Метаданные о сегменте.
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_index: usize,
    pub url: Url,
    pub duration: Option<Duration>,
}

/// Полезная нагрузка между слоями.
#[derive(Debug, Clone)]
pub struct SegmentPayload {
    pub meta: SegmentMeta,
    pub bytes: Bytes,
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("hls error: {0}")]
    Hls(#[from] HlsError),

    #[error("pipeline aborted")]
    Aborted,
}

pub type PipelineResult<T> = Result<T, PipelineError>;

/// Трейт для слоя: поток сегментов плюс доступ к общим каналам команд и событий.
pub trait SegmentStream: Stream<Item = PipelineResult<SegmentPayload>> + Send + 'static {
    fn command_sender(&self) -> mpsc::Sender<PipelineCommand>;
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent>;
}
