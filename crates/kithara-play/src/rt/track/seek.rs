use super::*;

impl PlayerResource {
    pub(crate) fn presentation_source_end(
        &self,
        sample_rate: NonZeroU32,
    ) -> Option<(SourceEnd, u64)> {
        let source_end = self.last_source_end?;
        (source_end.sample_rate() == sample_rate
            && source_end.sample_rate() == self.resource.get().spec().sample_rate)
            .then_some((source_end, self.last_warp_map_revision))
    }

    pub(in crate::rt::track) fn sync_render_revision(
        &mut self,
        activation: RenderActivation,
        required_frames: NonZeroUsize,
    ) -> RevisionFloorStatus {
        let revision = activation.revision;
        if revision <= self.render_revision_floor {
            return RevisionFloorStatus::Current;
        }
        let status = self.resource.get_mut().sync_render_revision(
            revision,
            required_frames,
            self.last_source_end,
        );
        if matches!(
            status,
            RevisionFloorStatus::WaitingForReplacement
                | RevisionFloorStatus::ReadyForSeekPresentation
        ) {
            return status;
        }
        self.render_revision_floor = revision;
        if self.source_spans.iter().any(|span| {
            span.source
                .is_some_and(|source| source.render_revision() < revision)
        }) {
            self.source_spans.clear();
            self.write_len = 0;
            self.write_pos = 0;
        }
        status
    }

    pub(crate) fn present_scheduled_seek(&mut self) -> ScheduledSeekPresentation {
        let Some(record) = self.scheduled_seek else {
            return ScheduledSeekPresentation::NoRequest;
        };
        match self.resource.get_mut().present_seek(record.decoder_epoch) {
            kithara_audio::SeekPresentation::Presented
            | kithara_audio::SeekPresentation::Current => {
                self.scheduled_seek = None;
                self.source_spans.clear();
                self.write_len = 0;
                self.write_pos = 0;
                self.last_source_end = None;
                self.eof_seen = false;
                self.failed = false;
                ScheduledSeekPresentation::Presented(record.disposition)
            }
            kithara_audio::SeekPresentation::Superseded => {
                self.scheduled_seek = None;
                ScheduledSeekPresentation::Superseded
            }
        }
    }

    pub(crate) fn schedule_seek(
        &mut self,
        scheduled_epoch: ScheduledSeekEpoch,
        decoder_epoch: u64,
        disposition: ScheduledSeekDisposition,
        armed: bool,
    ) -> bool {
        if let Some(latest) = self.latest_scheduled_epoch {
            if scheduled_epoch < latest {
                return false;
            }
            if scheduled_epoch == latest {
                return true;
            }
        }
        self.latest_scheduled_epoch = Some(scheduled_epoch);
        self.scheduled_seek = Some(ScheduledSeekRecord {
            scheduled_epoch,
            decoder_epoch,
            disposition,
            armed,
        });
        true
    }

    /// The disposition of the seek that is still waiting for presentation.
    ///
    /// Eligibility is derived from this existing scheduled operation. The
    /// renderer must not infer a launch from the track's audible state.
    #[must_use]
    pub(crate) fn scheduled_seek_disposition(&self) -> Option<ScheduledSeekDisposition> {
        self.scheduled_seek.map(|record| record.disposition)
    }

    pub(crate) fn is_armed_prepared_launch(&self) -> bool {
        matches!(
            self.scheduled_seek,
            Some(ScheduledSeekRecord {
                disposition: ScheduledSeekDisposition::PreparedLaunch(_),
                armed: true,
                ..
            })
        )
    }

    pub(crate) fn set_prepared_launch_armed(
        &mut self,
        scheduled_epoch: ScheduledSeekEpoch,
    ) -> bool {
        let Some(mut record) = self.scheduled_seek else {
            return false;
        };
        if record.scheduled_epoch != scheduled_epoch || !record.disposition.is_prepared_launch() {
            return false;
        }
        record.armed = true;
        self.scheduled_seek = Some(record);
        true
    }

    pub(crate) fn disarm_prepared_launch(&mut self) -> bool {
        let Some(mut record) = self.scheduled_seek else {
            return false;
        };
        if !record.disposition.is_prepared_launch() {
            return false;
        }
        record.armed = false;
        self.scheduled_seek = Some(record);
        true
    }

    pub(crate) fn has_prepared_launch(&self, scheduled_epoch: ScheduledSeekEpoch) -> bool {
        let Some(record) = self.scheduled_seek else {
            return false;
        };
        record.scheduled_epoch == scheduled_epoch && record.disposition.is_prepared_launch()
    }

    pub(crate) fn matches_prepared_launch(
        &self,
        scheduled_epoch: ScheduledSeekEpoch,
        decoder_epoch: u64,
    ) -> bool {
        self.scheduled_seek.is_some_and(|record| {
            record.scheduled_epoch == scheduled_epoch
                && record.decoder_epoch == decoder_epoch
                && record.disposition.is_prepared_launch()
        })
    }

    pub(crate) fn present_replacement_prepared_launch(
        &mut self,
        scheduled_epoch: ScheduledSeekEpoch,
        prepared_epoch: u64,
        replacement_epoch: u64,
    ) -> bool {
        if !self.matches_prepared_launch(scheduled_epoch, prepared_epoch) {
            return false;
        }
        match self.resource.get_mut().present_seek(replacement_epoch) {
            kithara_audio::SeekPresentation::Presented
            | kithara_audio::SeekPresentation::Current => true,
            kithara_audio::SeekPresentation::Superseded => false,
        }
    }

    pub(crate) fn clear_prepared_launch(&mut self, scheduled_epoch: ScheduledSeekEpoch) {
        if self.has_prepared_launch(scheduled_epoch) {
            self.scheduled_seek = None;
        }
    }
}
