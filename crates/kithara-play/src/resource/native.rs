use kithara_audio::{
    SourceAudioActivity, SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome,
    SourceAudioReader, SourceFrameRange,
};

use super::Resource;

impl Resource {
    pub(crate) fn activate_source_audio_authoritative(&mut self) -> Result<bool, SourceAudioError> {
        let Some(source_audio) = self.source_audio.as_mut() else {
            return Ok(false);
        };
        source_audio.activate_authoritative(self.inner.spec())?;
        Ok(true)
    }

    pub(crate) fn deactivate_source_audio(&mut self) -> Result<(), SourceAudioError> {
        if let Some(source_audio) = self.source_audio.as_mut() {
            source_audio.deactivate()?;
            source_audio.poll();
        }
        Ok(())
    }

    pub(crate) fn read_source_audio(
        &mut self,
        demand: &SourceAudioDemand,
        range: SourceFrameRange,
        output: &mut [f32],
    ) -> Result<Option<SourceAudioReadOutcome>, SourceAudioError> {
        self.source_audio
            .as_mut()
            .map(|source_audio| source_audio.read_range_into(demand, range, output))
            .transpose()
    }

    pub(crate) fn request_source_audio(
        &mut self,
        range: SourceFrameRange,
        look_ahead_frames: u64,
    ) -> Result<Option<SourceAudioDemand>, SourceAudioError> {
        let Some(source_audio) = self.source_audio.as_mut() else {
            return Ok(None);
        };
        source_audio
            .request(range, look_ahead_frames, self.inner.preload_epoch())
            .map(Some)
    }

    pub(crate) const fn supports_reverse_source(&self) -> bool {
        self.supports_reverse_source
    }

    pub(crate) fn take_source_audio_activity(&mut self) -> Option<SourceAudioActivity> {
        self.source_audio
            .as_mut()
            .and_then(SourceAudioReader::take_activity)
    }
}
