use std::sync::atomic::{AtomicU32, Ordering};

use kithara_decode::{DecoderSeekOutcome, PcmChunk, duration_for_frames};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{SourceSeekAnchor, StreamType};
use tracing::warn;

use crate::pipeline::{
    decode::DecoderSession,
    rebuild::{RecreateCause, RecreateNext, RecreateState},
    seek::{SeekContext, SeekEngine, SeekRequest},
    stream::shared::SharedStream,
};

pub(crate) struct ResumeCursor {
    decode_head: Option<(u64, u64, u32)>,
    host_rate: Arc<AtomicU32>,
    recreate_on_route: bool,
    decoder_rate: u32,
}

pub(crate) struct RouteCtx<'a, T: StreamType> {
    pub(crate) committed: Duration,
    pub(crate) seek: &'a SeekEngine,
    pub(crate) seek_active: bool,
    pub(crate) session: &'a DecoderSession,
    pub(crate) stream: &'a SharedStream<T>,
}

pub(crate) fn seek_position(outcome: DecoderSeekOutcome) -> Duration {
    match outcome {
        DecoderSeekOutcome::Landed { landed_at, .. } => landed_at,
        DecoderSeekOutcome::PastEof { duration } => duration,
    }
}

impl ResumeCursor {
    pub(crate) fn new(
        host_rate: Arc<AtomicU32>,
        recreate_on_route: bool,
        decoder_rate: u32,
    ) -> Self {
        Self {
            decode_head: None,
            host_rate,
            recreate_on_route,
            decoder_rate,
        }
    }

    pub(crate) fn record(&mut self, chunk: &PcmChunk, epoch: u64) {
        self.decode_head = Some((
            epoch,
            chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            chunk.meta.spec.sample_rate.get(),
        ));
    }

    pub(crate) fn resume_position(
        &self,
        epoch: u64,
        committed: Duration,
        resume_target: Option<(u64, Duration)>,
    ) -> Duration {
        let head = self
            .decode_head
            .filter(|&(head_epoch, _, _)| head_epoch == epoch)
            .map(|(_, frame, rate)| duration_for_frames(rate, frame))
            .filter(|&position| position > committed)
            .unwrap_or(committed);
        match resume_target {
            Some((target_epoch, target)) if target_epoch == epoch && target > head => target,
            _ => head,
        }
    }

    #[cfg(test)]
    pub(crate) fn decoder_rate(&self) -> u32 {
        self.decoder_rate
    }

    pub(crate) fn host_rate(&self) -> u32 {
        self.host_rate.load(Ordering::Acquire)
    }

    pub(crate) fn recreates_on_route(&self) -> bool {
        self.recreate_on_route
    }

    pub(crate) fn route_change<T: StreamType>(
        &mut self,
        ctx: &RouteCtx<'_, T>,
    ) -> Option<RecreateState> {
        if !self.recreate_on_route || ctx.seek_active {
            return None;
        }
        let host_rate = self.host_rate.load(Ordering::Acquire);
        if host_rate == 0 {
            return None;
        }
        if self.decoder_rate == 0 && ctx.session.decoder.spec().sample_rate.get() == host_rate {
            self.decoder_rate = host_rate;
            return None;
        }
        if host_rate == self.decoder_rate {
            return None;
        }
        let media_info = ctx
            .session
            .media_info
            .clone()
            .or_else(|| ctx.stream.media_info())?;
        let epoch = ctx.seek.epoch();
        let target = self.resume_position(epoch, ctx.committed, None);
        let offset = match ctx.stream.seek_time_anchor(target) {
            Ok(Some(SourceSeekAnchor { byte_offset, .. })) => byte_offset,
            Ok(None) => ctx.session.base_offset,
            Err(err) => {
                warn!(
                    ?err,
                    ?target,
                    "route-change recreate anchor resolution failed"
                );
                ctx.session.base_offset
            }
        };
        self.decoder_rate = host_rate;
        Some(RecreateState {
            cause: RecreateCause::RouteChange,
            media_info,
            next: RecreateNext::ApplySeek(SeekRequest {
                seek: SeekContext { target, epoch },
                emit_request: false,
            }),
            offset,
        })
    }
}
