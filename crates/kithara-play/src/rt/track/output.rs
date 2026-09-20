use super::*;

impl PlayerResource {
    pub(crate) fn render_reader(&self) -> Option<RenderReader> {
        self.resource.get().render_reader()
    }

    /// Drop everything buffered ahead of a seek the control thread began. Lock-free: the reader
    /// picks up the epoch itself via `sync_seek`.
    pub fn reset_for_seek(&mut self) {
        self.resource.get_mut().defer_seek_until_pcm();
        self.write_len = 0;
        self.write_pos = 0;
        self.source_spans.clear();
        self.last_source_end = None;
        self.resource.get().clear_render();
        self.eof_seen = false;
        self.failed = false;
        self.activation_blend_pos = self.activation_blend_frames;
    }

    pub(super) const fn scratch_frames(sample_rate: u32) -> FrameCount {
        FrameCount::new(sample_rate as usize / Self::BUFFER_DURATION_DIVISOR)
    }

    /// Control-plane handle used to begin a seek off the audio thread.
    #[must_use]
    pub fn seek_handle(&self) -> Option<Arc<dyn kithara_audio::SeekBegin>> {
        self.resource.get().seek_handle()
    }

    delegate::delegate! {
        to self.resource.get() {
            /// Total duration in seconds. Returns 0.0 if unknown.
            #[must_use]
            #[expr($.map_or(0.0, |d| d.as_secs_f64()))]
            pub fn duration(&self) -> f64;
            /// Set the target sample rate of the audio host.
            pub(crate) fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Update the scheduling priority hint for the shared worker.
            pub(crate) fn set_service_class(&self, class: ServiceClass);
            pub(crate) fn clear_render(&self);
            pub(crate) fn publish_render(
                &self,
                context: &RenderContext,
                frontier: PresentationFrontier,
            );
            pub(crate) fn publish_render_preparation(&self, context: &RenderContext);
        }
    }
}
