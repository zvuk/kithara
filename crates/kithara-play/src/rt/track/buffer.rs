use super::*;

impl PlayerResource {
    pub(in crate::rt::track) fn consume_source(
        &mut self,
        mut frames: usize,
        output_offset: usize,
        context: Option<&RenderContext>,
    ) -> Option<u64> {
        let mut source_frames = 0u64;
        let mut output_start = output_offset;
        while frames > 0 {
            let mut span = self.source_spans.pop_front()?;
            let consumed = frames.min(span.remaining());
            let output_end = output_start.saturating_add(consumed);
            let (source, consumed_source_frames) = span.take(consumed);
            source_frames = source_frames.checked_add(consumed_source_frames)?;
            match (context, source) {
                (Some(context), Some(source)) => {
                    kithara::probe_event!(
                        pcm_consumed,
                        render_revision = source.render_revision(),
                        output_start = i64::from(context.output_frames().start)
                            .saturating_add(i64::try_from(output_start).unwrap_or(i64::MAX)),
                        output_end = i64::from(context.output_frames().start)
                            .saturating_add(i64::try_from(output_end).unwrap_or(i64::MAX)),
                        source_start = source.start(),
                        source_end = source.end()
                    );
                    self.last_source_end = Some(SourceEnd::new(source.end(), source.sample_rate()));
                    self.last_warp_map_revision =
                        kithara_signal::render_warp_map_revision(source.render_revision());
                }
                (None, Some(source)) => {
                    self.last_source_end = Some(SourceEnd::new(source.end(), source.sample_rate()));
                    self.last_warp_map_revision =
                        kithara_signal::render_warp_map_revision(source.render_revision());
                }
                (_, None) => {}
            }
            frames -= consumed;
            output_start = output_end;
            if span.remaining() > 0 {
                self.source_spans.push_front(span);
            }
        }
        Some(source_frames)
    }

    /// Decoded-ahead frontier in seconds: how much content has been decoded
    /// and is ready to play (always `>=` the served playback position).
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.resource.get().decoded_frontier().as_secs_f64()
    }

    /// Record one PCM underrun and silence the unfilled suffix of `range`.
    pub(in crate::rt::track) fn fill_underrun(
        &self,
        context: Option<&RenderContext>,
        track_id: Option<TrackId>,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        available_frames: usize,
        metrics: &RtMetrics,
    ) {
        metrics.record_underrun();
        kithara::probe_event!(
            pcm_underrun,
            track_id = track_id.map(TrackId::as_u64),
            output_start = context.map_or(0, |context| i64::from(context.output_frames().start)),
            requested_frames = range.len(),
            available_frames = available_frames,
            source_end = self.last_source_end.map(|source| source.frame())
        );
        for ch in output.iter_mut() {
            ch[range.start + available_frames..range.end].fill(0.0);
        }
    }

    pub(in crate::rt::track) fn fill_scratch(
        &mut self,
        target_frames: usize,
        metrics: &RtMetrics,
    ) -> bool {
        let mut eof_reached = self.eof_seen;

        while target_frames > self.write_len && !eof_reached {
            let needed = target_frames - self.write_len;
            let avail = (self.channel_buffers[0].len() - self.write_pos).min(needed);
            if avail == 0 {
                break;
            }

            let channel_buffers = &mut self.channel_buffers;
            let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
            let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
            let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
            let mut planar: [&mut [f32]; Self::STEREO_CHANNELS] = [left, right];

            let position_before = self.resource.get().position();
            let (n, position, source) = match self.resource.get_mut().read_planar(&mut planar) {
                Ok(kithara_audio::ReadOutcome::Frames {
                    count,
                    position,
                    source_span,
                }) => (count.get(), position, source_span),
                Ok(kithara_audio::ReadOutcome::Pending { position, .. }) => (0, position, None),
                Ok(kithara_audio::ReadOutcome::Eof { .. }) => {
                    self.eof_seen = true;
                    eof_reached = true;
                    (0, position_before, None)
                }
                Err(_) => {
                    metrics.record_decode_error();
                    self.failed = true;
                    (0, position_before, None)
                }
            };
            if n == 0 {
                break;
            }
            if source
                .is_some_and(|span| span.sample_rate() != self.resource.get().spec().sample_rate)
            {
                metrics.record_decode_error();
                self.failed = true;
                break;
            }
            let media_frames = source.map_or_else(
                || {
                    let spec = self.resource.get().spec();
                    spec.frame_at(position)
                        .ok()
                        .zip(spec.frame_at(position_before).ok())
                        .map_or(0, |(end, start)| end.saturating_sub(start))
                },
                |span| span.end().saturating_sub(span.start()),
            );
            self.source_spans.push_back(SourceWindow {
                source,
                frames: n,
                media_frames,
                consumed_frames: 0,
            });
            self.write_len += n;
            self.write_pos += n;
        }

        eof_reached
    }

    pub(in crate::rt::track) fn prefetch_target(&self, callback_frames: usize) -> usize {
        self.write_len
            .saturating_add(callback_frames)
            .min(self.channel_buffers[0].len())
    }

    /// Remaining buffered frames when the wrapped reader has reached EOF.
    ///
    /// `Some(0)` means the current read drained the last buffered frame exactly;
    /// the next read will return [`ReadOutcome::Eof`].
    #[must_use]
    pub fn frames_until_eof(&self) -> Option<usize> {
        self.eof_seen.then_some(self.write_len)
    }
}
