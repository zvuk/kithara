use std::sync::atomic::{AtomicU32, Ordering};

use kithara_decode::duration_for_frames;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::StreamType;

use crate::pipeline::{
    decode::DecoderGeneration,
    rebuild::{RecreateCause, RecreateNext, RecreateState},
    seek::{SeekContext, SeekEngine, SeekRequest, anchor},
    stream::shared::SharedStream,
    window::SourceEnd,
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ResumeCursor {
    host_rate: Arc<AtomicU32>,
    decode_head: Option<(u64, u64, u32)>,
    #[field(get = recreates_on_route, vis = "pub(crate)")]
    recreate_on_route: bool,
    decoder_rate: u32,
}

pub(crate) struct RouteCtx<'a, T: StreamType> {
    pub(crate) active: &'a DecoderGeneration,
    pub(crate) seek: &'a SeekEngine,
    pub(crate) stream: &'a SharedStream<T>,
    pub(crate) committed: Duration,
    pub(crate) seek_active: bool,
}

impl ResumeCursor {
    pub(crate) fn new(
        host_rate: Arc<AtomicU32>,
        recreate_on_route: bool,
        decoder_rate: u32,
    ) -> Self {
        Self {
            host_rate,
            recreate_on_route,
            decoder_rate,
            decode_head: None,
        }
    }

    pub(crate) fn decode_head(&self, epoch: u64) -> Option<(u64, u32)> {
        self.decode_head
            .filter(|&(head_epoch, _, _)| head_epoch == epoch)
            .map(|(_, frame, rate)| (frame, rate))
    }

    #[cfg(test)]
    pub(crate) fn decoder_rate(&self) -> u32 {
        self.decoder_rate
    }

    pub(crate) fn host_rate(&self) -> u32 {
        self.host_rate.load(Ordering::Acquire)
    }

    pub(crate) fn record(&mut self, source_end: Option<SourceEnd>, epoch: u64) {
        if let Some(source_end) = source_end {
            self.decode_head = Some((epoch, source_end.frame, source_end.rate));
        }
    }

    pub(crate) fn resume_position(
        &self,
        epoch: u64,
        committed: Duration,
        resume_target: Option<(u64, Duration)>,
    ) -> Duration {
        let head = self
            .decode_head(epoch)
            .map(|(frame, rate)| duration_for_frames(rate, frame))
            .filter(|&position| position > committed)
            .unwrap_or(committed);
        match resume_target {
            Some((target_epoch, target)) if target_epoch == epoch && target > head => target,
            _ => head,
        }
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
        if self.decoder_rate == 0 && ctx.active.decoder().spec().sample_rate.get() == host_rate {
            self.decoder_rate = host_rate;
            return None;
        }
        if host_rate == self.decoder_rate {
            return None;
        }
        let media_info = ctx
            .active
            .media_info()
            .cloned()
            .or_else(|| ctx.stream.media_info())?;
        // A route change keeps the container, so the rebuilt demuxer must start
        // where the container starts — not at the byte the resume time maps to.
        // Seeking the anchor by time lands past the init and the demuxer never
        let offset = anchor::recreate_offset(
            ctx.stream,
            media_info.container,
            false,
            ctx.active.base_offset(),
        )?;
        let epoch = ctx.seek.epoch();
        let target = self.resume_position(epoch, ctx.committed, None);
        self.decoder_rate = host_rate;
        Some(RecreateState {
            media_info,
            offset,
            cause: RecreateCause::RouteChange,
            next: RecreateNext::ApplySeek(SeekRequest {
                seek: SeekContext { target, epoch },
                emit_request: false,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn missing_source_end_preserves_the_last_proven_head() {
        let mut cursor = ResumeCursor::new(Arc::new(AtomicU32::new(48_000)), false, 48_000);
        cursor.record(
            Some(SourceEnd {
                frame: 512,
                rate: 48_000,
            }),
            7,
        );

        cursor.record(None, 7);

        assert_eq!(cursor.decode_head(7), Some((512, 48_000)));
    }
}
