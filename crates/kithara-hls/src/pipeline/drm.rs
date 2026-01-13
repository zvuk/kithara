use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::Stream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentPayload};
use crate::{
    HlsError, HlsResult,
    keys::KeyManager,
    playlist::{EncryptionMethod, KeyInfo},
};

/// DRM-оверлей: расшифровывает сегменты (если задан decryptor), транслирует команды вниз,
/// публикует событие Decrypted, реагирует на CancellationToken.
pub struct DrmStream<I>
where
    I: PipelineStream,
{
    inner: Pin<Box<I>>,
    events: broadcast::Sender<PipelineEvent>,
    key_manager: Option<Arc<KeyManager>>,
    pending: Option<Pin<Box<dyn Future<Output = PipelineResult<SegmentPayload>> + Send>>>,
    cancel: CancellationToken,
}

impl<I> DrmStream<I>
where
    I: PipelineStream,
{
    pub fn new(inner: I, key_manager: Option<Arc<KeyManager>>, cancel: CancellationToken) -> Self {
        let events = inner.event_sender();

        Self {
            inner: Box::pin(inner),
            events,
            key_manager,
            pending: None,
            cancel,
        }
    }

    fn decrypt_future(
        &self,
        payload: SegmentPayload,
    ) -> Pin<Box<dyn Future<Output = PipelineResult<SegmentPayload>> + Send>> {
        let key_manager = self.key_manager.clone();
        Box::pin(async move { Self::decrypt_payload(key_manager, payload).await })
    }

    async fn decrypt_payload(
        key_manager: Option<Arc<KeyManager>>,
        payload: SegmentPayload,
    ) -> PipelineResult<SegmentPayload> {
        let Some(seg_key) = payload.meta.key.clone() else {
            return Ok(payload);
        };

        if !matches!(seg_key.method, EncryptionMethod::Aes128) {
            return Ok(payload);
        }

        let Some(key_info) = seg_key.key_info else {
            return Ok(payload);
        };

        let key_url =
            Self::resolve_key_url(&key_info, &payload.meta.url).map_err(PipelineError::Hls)?;
        let iv = Self::derive_iv(&key_info, payload.meta.sequence);

        let key_manager = match key_manager {
            Some(km) => km,
            None => return Ok(payload),
        };

        let bytes = key_manager
            .decrypt(&key_url, Some(iv), payload.bytes)
            .await
            .map_err(PipelineError::Hls)?;

        Ok(SegmentPayload {
            meta: payload.meta,
            bytes,
        })
    }

    fn resolve_key_url(key_info: &KeyInfo, segment_url: &Url) -> HlsResult<Url> {
        let key_uri = key_info
            .uri
            .as_ref()
            .ok_or_else(|| HlsError::InvalidUrl("missing key URI".to_string()))?;

        if key_uri.starts_with("http://") || key_uri.starts_with("https://") {
            Url::parse(key_uri).map_err(|e| HlsError::InvalidUrl(format!("Invalid key URL: {e}")))
        } else {
            segment_url
                .join(key_uri)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve key URL: {e}")))
        }
    }

    fn derive_iv(key_info: &KeyInfo, sequence: u64) -> [u8; 16] {
        if let Some(iv) = key_info.iv {
            return iv;
        }

        let mut iv = [0u8; 16];
        iv[8..].copy_from_slice(&sequence.to_be_bytes());
        iv
    }
}

impl<I> Stream for DrmStream<I>
where
    I: PipelineStream,
{
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.cancel.is_cancelled() {
                return Poll::Ready(Some(Err(PipelineError::Aborted)));
            }

            if let Some(fut) = self.pending.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(res) => {
                        self.pending = None;
                        if let Ok(ref payload) = res {
                            let _ = self.events.send(PipelineEvent::Decrypted {
                                variant: payload.meta.variant,
                                segment_index: payload.meta.segment_index,
                            });
                        }
                        return Poll::Ready(Some(res));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            let next = self.inner.as_mut().poll_next(cx);
            let payload = match next {
                Poll::Ready(Some(Ok(payload))) => payload,
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Some(Err(err))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            };

            if self.key_manager.is_some() && payload.meta.key.is_some() {
                self.pending = Some(self.decrypt_future(payload));
                continue;
            }

            let _ = self.events.send(PipelineEvent::Decrypted {
                variant: payload.meta.variant,
                segment_index: payload.meta.segment_index,
            });
            return Poll::Ready(Some(Ok(payload)));
        }
    }
}

impl<I> PipelineStream for DrmStream<I>
where
    I: PipelineStream,
{
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
