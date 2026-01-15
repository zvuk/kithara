//! SegmentStream: base layer that selects variant, iterates segments,
//! responds to commands (seek/force), and publishes events.

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
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    commands::StreamCommand,
    context::StreamContext,
    pipeline::create_stream,
    types::{PipelineEvent, PipelineResult, PipelineStream, SegmentMeta},
};
use crate::{abr::AbrController, fetch::FetchManager, keys::KeyManager, playlist::PlaylistManager};

/// Base layer: selects variant, iterates segments, decrypts when needed,
/// responds to commands (seek/force), publishes events.
pub struct SegmentStream {
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<StreamCommand>,
    current_variant: Arc<AtomicUsize>,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>>,
}

impl SegmentStream {
    pub fn new(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        key_manager: Option<Arc<KeyManager>>,
        abr_controller: AbrController,
        cancel: CancellationToken,
    ) -> Self {
        let (events, _) = broadcast::channel(32);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let current_variant = abr_controller.current_variant();

        let ctx = StreamContext {
            master_url,
            fetch,
            playlist_manager,
            _key_manager: key_manager,
            abr: abr_controller,
            cancel,
        };

        let inner = create_stream(ctx, events.clone(), cmd_rx);

        Self {
            events,
            cmd_tx,
            current_variant,
            inner,
        }
    }

    /// Seek to a specific segment index.
    pub fn seek(&self, segment_index: usize) {
        let _ = self.cmd_tx.try_send(StreamCommand::Seek { segment_index });
    }

    /// Force switch to a specific variant.
    /// Note: Event will be emitted by pipeline after successful variant load.
    pub fn force_variant(&self, variant_index: usize) {
        let from = self.current_variant.load(Ordering::SeqCst);
        let _ = self.cmd_tx.try_send(StreamCommand::ForceVariant {
            variant_index,
            from,
        });
    }

    /// Get current variant index.
    pub fn current_variant(&self) -> usize {
        self.current_variant.load(Ordering::SeqCst)
    }
}

impl Stream for SegmentStream {
    type Item = PipelineResult<SegmentMeta>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

impl PipelineStream for SegmentStream {
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
