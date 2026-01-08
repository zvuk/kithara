use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::sync::{broadcast, mpsc, Mutex};
use url::Url;

use crate::{
    abr::AbrController,
    fetch::FetchManager,
    keys::KeyManager,
    playlist::MediaPlaylist,
    HlsError,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("hls error: {0}")]
    Hls(#[from] HlsError),

    #[error("pipeline aborted")]
    Aborted,
}

pub type PipelineResult<T> = Result<T, PipelineError>;

#[derive(Debug, Clone)]
pub enum PipelineCommand {
    Seek { segment_index: usize },
    ForceVariant { variant_index: usize },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    VariantDecided { from: usize, to: usize },
    SegmentReady { variant: usize, segment_index: usize },
    Decrypted { variant: usize, segment_index: usize },
}

#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_index: usize,
    pub url: Url,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct SegmentPayload {
    pub meta: SegmentMeta,
    pub bytes: Bytes,
}

pub trait Layer:
    Stream<Item = PipelineResult<SegmentPayload>> + Send + Unpin + 'static
{
    fn commands(&self) -> mpsc::Sender<PipelineCommand>;
    fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent>;
}

pub struct BaseLayer {
    stream: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    events_tx: broadcast::Sender<PipelineEvent>,
}

impl BaseLayer {
    pub fn new(
        fetch: Arc<FetchManager>,
        media_playlist: MediaPlaylist,
        media_url: Url,
        variant_index: usize,
    ) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(32);
        let (events_tx, _) = broadcast::channel(128);

        let mut segment_urls = Vec::with_capacity(media_playlist.segments.len());
        let mut durations = Vec::with_capacity(media_playlist.segments.len());

        for seg in media_playlist.segments.iter() {
            let url = media_url
                .join(&seg.uri)
                .unwrap_or_else(|_| media_url.clone());
            segment_urls.push(url);
            durations.push(Some(seg.duration));
        }

        let enumerated = fetch
            .stream_segment_sequence(media_playlist, &media_url, None)
            .enumerate();

        let stream_events = events_tx.clone();
        let stream = stream! {
            let mut stream = enumerated;
            let mut skip_until: usize = 0;
            let mut shutdown = false;

            while !shutdown {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            PipelineCommand::Seek { segment_index } => {
                                skip_until = segment_index;
                            }
                            PipelineCommand::ForceVariant { .. } => {
                                // Base layer is variant-agnostic; forwarding happens in upper layers.
                            }
                            PipelineCommand::Shutdown => {
                                shutdown = true;
                            }
                        }
                    }
                    item = stream.next() => {
                        match item {
                            Some((idx, result)) => {
                                match result {
                                    Ok(bytes) => {
                                        if idx < skip_until {
                                            continue;
                                        }

                                        let url = segment_urls
                                            .get(idx)
                                            .cloned()
                                            .unwrap_or_else(|| media_url.clone());
                                        let meta = SegmentMeta {
                                            variant: variant_index,
                                            segment_index: idx,
                                            url,
                                            duration: durations.get(idx).cloned().flatten(),
                                        };

                                        let _ = stream_events.send(PipelineEvent::SegmentReady {
                                            variant: meta.variant,
                                            segment_index: meta.segment_index,
                                        });

                                        yield Ok(SegmentPayload { meta, bytes });
                                    }
                                    Err(err) => {
                                        yield Err(PipelineError::Hls(err));
                                    }
                                }
                            }
                            None => {
                                break;
                            }
                        }
                    }
                }
            }
        };

        Self {
            stream: Box::pin(stream),
            cmd_tx,
            events_tx,
        }
    }
}

impl Stream for BaseLayer {
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.stream.as_mut().poll_next(cx)
    }
}

impl Layer for BaseLayer {
    fn commands(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent> {
        self.events_tx.subscribe()
    }
}

pub struct AbrLayer<L>
where
    L: Layer,
{
    stream: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    events_tx: broadcast::Sender<PipelineEvent>,
    _inner_events_tx: broadcast::Sender<PipelineEvent>,
}

impl<L> AbrLayer<L>
where
    L: Layer,
{
    pub fn new(inner: L, abr: Arc<Mutex<AbrController>>) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(32);
        let (events_tx, _) = broadcast::channel(128);
        let inner_cmd = inner.commands();
        let mut inner_events = inner.subscribe_events();
        let events_out = events_tx.clone();
        let inner_events_tx = events_tx.clone();

        let stream = stream! {
            let mut inner_stream = inner;
            let mut current_variant = {
                let guard = abr.lock().await;
                guard.current_variant()
            };
            let mut shutdown = false;

            while !shutdown {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            PipelineCommand::Seek { segment_index } => {
                                let _ = inner_cmd.send(PipelineCommand::Seek { segment_index }).await;
                            }
                            PipelineCommand::ForceVariant { variant_index } => {
                                let from = current_variant;
                                current_variant = variant_index;
                                {
                                    let mut guard = abr.lock().await;
                                    // The controller keeps internal history; here we only update desired variant.
                                    guard.set_current_variant(variant_index);
                                }
                                let _ = inner_cmd.send(PipelineCommand::ForceVariant { variant_index }).await;
                                let _ = events_out.send(PipelineEvent::VariantDecided { from, to: variant_index });
                            }
                            PipelineCommand::Shutdown => {
                                shutdown = true;
                                let _ = inner_cmd.send(PipelineCommand::Shutdown).await;
                            }
                        }
                    }
                    evt = inner_events.recv() => {
                        if let Ok(evt) = evt {
                            let _ = inner_events_tx.send(evt);
                        } else {
                            // channel closed; continue
                        }
                    }
                    item = inner_stream.next() => {
                        match item {
                            Some(Ok(payload)) => {
                                let _ = inner_events_tx.send(PipelineEvent::SegmentReady {
                                    variant: payload.meta.variant,
                                    segment_index: payload.meta.segment_index,
                                });
                                yield Ok(payload);
                            }
                            Some(Err(err)) => {
                                yield Err(err);
                            }
                            None => {
                                break;
                            }
                        }
                    }
                }
            }
        };

        Self {
            stream: Box::pin(stream),
            cmd_tx,
            events_tx,
            _inner_events_tx: inner_events_tx,
        }
    }
}

impl<L> Stream for AbrLayer<L>
where
    L: Layer,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.stream.as_mut().poll_next(cx)
    }
}

impl<L> Layer for AbrLayer<L>
where
    L: Layer,
{
    fn commands(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent> {
        self.events_tx.subscribe()
    }
}

pub struct DrmLayer<L>
where
    L: Layer,
{
    stream: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    events_tx: broadcast::Sender<PipelineEvent>,
}

impl<L> DrmLayer<L>
where
    L: Layer,
{
    pub fn new(inner: L, _key_manager: Option<Arc<KeyManager>>) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(32);
        let (events_tx, _) = broadcast::channel(128);
        let inner_cmd = inner.commands();
        let mut inner_events = inner.subscribe_events();
        let events_out = events_tx.clone();

        let stream = stream! {
            let mut inner_stream = inner;
            let mut shutdown = false;

            while !shutdown {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            PipelineCommand::Seek { segment_index } => {
                                let _ = inner_cmd.send(PipelineCommand::Seek { segment_index }).await;
                            }
                            PipelineCommand::ForceVariant { variant_index } => {
                                let _ = inner_cmd.send(PipelineCommand::ForceVariant { variant_index }).await;
                            }
                            PipelineCommand::Shutdown => {
                                shutdown = true;
                                let _ = inner_cmd.send(PipelineCommand::Shutdown).await;
                            }
                        }
                    }
                    evt = inner_events.recv() => {
                        if let Ok(evt) = evt {
                            let _ = events_out.send(evt);
                        }
                    }
                    item = inner_stream.next() => {
                        match item {
                            Some(Ok(mut payload)) => {
                                // DRM processing is currently a passthrough; hook for future decrypt logic.
                                let _ = events_out.send(PipelineEvent::Decrypted {
                                    variant: payload.meta.variant,
                                    segment_index: payload.meta.segment_index,
                                });
                                yield Ok(payload);
                            }
                            Some(Err(err)) => {
                                yield Err(err);
                            }
                            None => {
                                break;
                            }
                        }
                    }
                }
            }
        };

        Self {
            stream: Box::pin(stream),
            cmd_tx,
            events_tx,
        }
    }
}

impl<L> Stream for DrmLayer<L>
where
    L: Layer,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.stream.as_mut().poll_next(cx)
    }
}

impl<L> Layer for DrmLayer<L>
where
    L: Layer,
{
    fn commands(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent> {
        self.events_tx.subscribe()
    }
}

pub struct PrefetchLayer<L>
where
    L: Layer,
{
    stream: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
    cmd_tx: mpsc::Sender<PipelineCommand>,
    events_tx: broadcast::Sender<PipelineEvent>,
}

impl<L> PrefetchLayer<L>
where
    L: Layer,
{
    pub fn new(inner: L, capacity: usize) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(32);
        let (events_tx, _) = broadcast::channel(128);
        let inner_cmd = inner.commands();
        let mut inner_events = inner.subscribe_events();
        let events_out = events_tx.clone();
        let cap = capacity.max(1);

        let stream = stream! {
            let mut inner_stream = inner;
            let mut buf: VecDeque<PipelineResult<SegmentPayload>> = VecDeque::with_capacity(cap);
            let mut shutdown = false;

            loop {
                while buf.len() < cap && !shutdown {
                    tokio::select! {
                        Some(cmd) = cmd_rx.recv() => {
                            match cmd {
                                PipelineCommand::Seek { segment_index } => {
                                    let _ = inner_cmd.send(PipelineCommand::Seek { segment_index }).await;
                                    buf.clear();
                                }
                                PipelineCommand::ForceVariant { variant_index } => {
                                    let _ = inner_cmd.send(PipelineCommand::ForceVariant { variant_index }).await;
                                }
                                PipelineCommand::Shutdown => {
                                    shutdown = true;
                                    let _ = inner_cmd.send(PipelineCommand::Shutdown).await;
                                }
                            }
                        }
                        evt = inner_events.recv() => {
                            if let Ok(evt) = evt {
                                let _ = events_out.send(evt);
                            }
                        }
                        item = inner_stream.next(), if !shutdown => {
                            match item {
                                Some(res) => buf.push_back(res),
                                None => shutdown = true,
                            }
                        }
                    }
                }

                if let Some(item) = buf.pop_front() {
                    yield item;
                } else {
                    break;
                }
            }
        };

        Self {
            stream: Box::pin(stream),
            cmd_tx,
            events_tx,
        }
    }
}

impl<L> Stream for PrefetchLayer<L>
where
    L: Layer,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.stream.as_mut().poll_next(cx)
    }
}

impl<L> Layer for PrefetchLayer<L>
where
    L: Layer,
{
    fn commands(&self) -> mpsc::Sender<PipelineCommand> {
        self.cmd_tx.clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent> {
        self.events_tx.subscribe()
    }
}

pub struct Pipeline {
    root: PrefetchLayer<DrmLayer<AbrLayer<BaseLayer>>>,
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fetch: Arc<FetchManager>,
        abr: Arc<Mutex<AbrController>>,
        key_manager: Option<Arc<KeyManager>>,
        media_playlist: MediaPlaylist,
        media_url: Url,
        variant_index: usize,
        prefetch: usize,
    ) -> Self {
        let base = BaseLayer::new(fetch, media_playlist, media_url, variant_index);
        let abr_layer = AbrLayer::new(base, abr);
        let drm_layer = DrmLayer::new(abr_layer, key_manager);
        let prefetch_layer = PrefetchLayer::new(drm_layer, prefetch);

        Self {
            root: prefetch_layer,
        }
    }

    pub fn commands(&self) -> mpsc::Sender<PipelineCommand> {
        self.root.commands()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent> {
        self.root.subscribe_events()
    }
}

impl Stream for Pipeline {
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Pin::new(&mut this.root).poll_next(cx)
    }
}

impl Layer for Pipeline {
    fn commands(&self) -> mpsc::Sender<PipelineCommand> {
        self.root.commands()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PipelineEvent> {
        self.root.subscribe_events()
    }
}
