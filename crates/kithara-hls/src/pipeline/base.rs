use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use async_stream::stream;
use futures::{Stream, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentMeta, SegmentPayload,
    SegmentStream,
};
use crate::{
    HlsError,
    fetch::SegmentStream as FetchSegmentStream,
    playlist::{MasterPlaylist, MediaPlaylist},
};

/// Абстракция получения сегментов выбранного варианта в виде Stream<Bytes>.
pub trait Fetcher: Send + Sync + 'static {
    fn stream_segment_sequence(
        &self,
        media_playlist: MediaPlaylist,
        base_url: &Url,
    ) -> FetchSegmentStream<'static>;
}

/// Абстракция доступа к мастер/медиа плейлистам.
pub trait PlaylistProvider: Send + Sync + 'static {
    fn master_playlist(&self) -> &MasterPlaylist;
    fn media_playlist(&self, variant_index: usize) -> Option<(Url, MediaPlaylist)>;
}

/// Базовый слой: выбирает вариант, итерирует сегменты, реагирует на команды (seek/force/shutdown),
/// публикует события в общий канал.
pub struct BaseStream<F, P>
where
    F: Fetcher,
    P: PlaylistProvider,
{
    fetcher: Arc<F>,
    playlists: Arc<P>,
    cancel: CancellationToken,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    cmd_rx: mpsc::Receiver<PipelineCommand>,
    events: broadcast::Sender<PipelineEvent>,
    current_variant: Arc<AtomicUsize>,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
}

impl<F, P> BaseStream<F, P>
where
    F: Fetcher,
    P: PlaylistProvider,
{
    pub fn new(
        fetcher: Arc<F>,
        playlists: Arc<P>,
        cancel: CancellationToken,
        initial_variant: usize,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let current_variant = Arc::new(AtomicUsize::new(initial_variant));

        let inner = Self::variant_stream(
            fetcher.clone(),
            playlists.clone(),
            events.clone(),
            initial_variant,
            0,
        );

        Self {
            fetcher,
            playlists,
            cancel,
            cmd_tx,
            cmd_rx,
            events,
            current_variant,
            inner,
        }
    }

    fn variant_stream(
        fetcher: Arc<F>,
        playlists: Arc<P>,
        events: broadcast::Sender<PipelineEvent>,
        variant_index: usize,
        start_from: usize,
    ) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>> {
        let playlist_pair = playlists.media_playlist(variant_index);
        let Some((media_url, media_playlist)) = playlist_pair else {
            return Box::pin(stream! {
                yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!("variant {}", variant_index))));
            });
        };

        let mut enumerated = fetcher
            .stream_segment_sequence(media_playlist, &media_url)
            .enumerate();

        Box::pin(stream! {
            while let Some((idx, item)) = enumerated.next().await {
                if idx < start_from {
                    continue;
                }

                match item {
                    Ok(bytes) => {
                        let meta = SegmentMeta {
                            variant: variant_index,
                            segment_index: idx,
                            url: media_url.clone(),
                            duration: None,
                        };
                        let _ = events.send(PipelineEvent::VariantApplied {
                            from: variant_index,
                            to: variant_index,
                        });
                        let _ = events.send(PipelineEvent::SegmentReady {
                            variant: variant_index,
                            segment_index: idx,
                        });
                        yield Ok(SegmentPayload { meta, bytes });
                    }
                    Err(err) => {
                        yield Err(PipelineError::Hls(err));
                        break;
                    }
                }
            }
        })
    }

    fn handle_commands(&mut self) -> Result<bool, PipelineError> {
        let mut changed = false;

        while let Ok(cmd) = self.cmd_rx.try_recv() {
            changed = true;
            match cmd {
                PipelineCommand::Seek { segment_index } => {
                    let variant = self.current_variant.load(Ordering::Relaxed);
                    self.inner = Self::variant_stream(
                        self.fetcher.clone(),
                        self.playlists.clone(),
                        self.events.clone(),
                        variant,
                        segment_index,
                    );
                }
                PipelineCommand::ForceVariant { variant_index } => {
                    let from = self.current_variant.swap(variant_index, Ordering::Relaxed);
                    let _ = self.events.send(PipelineEvent::VariantApplied {
                        from,
                        to: variant_index,
                    });
                    self.inner = Self::variant_stream(
                        self.fetcher.clone(),
                        self.playlists.clone(),
                        self.events.clone(),
                        variant_index,
                        0,
                    );
                }
                PipelineCommand::Shutdown => return Err(PipelineError::Aborted),
            }
        }

        Ok(changed)
    }
}

impl<F, P> Stream for BaseStream<F, P>
where
    F: Fetcher,
    P: PlaylistProvider,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.cancel.is_cancelled() {
            return Poll::Ready(Some(Err(PipelineError::Aborted)));
        }

        if let Err(err) = this.handle_commands() {
            return Poll::Ready(Some(Err(err)));
        }

        this.inner.as_mut().poll_next(cx)
    }
}

impl<F, P> SegmentStream for BaseStream<F, P>
where
    F: Fetcher,
    P: PlaylistProvider,
{
    fn command_sender(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
