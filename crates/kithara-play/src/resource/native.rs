use kithara_audio::{SourceRange, SourceRangeError, SourceRangeReadOutcome, SourceRangeRequest};

use super::Resource;

impl Resource {
    delegate::delegate! {
        to self.inner {
            pub(crate) fn request_source_range(
                &mut self,
                range: SourceRange,
            ) -> Result<SourceRangeRequest, SourceRangeError>;
            pub(crate) fn read_source_range(
                &mut self,
                request: SourceRangeRequest,
                output: &mut [f32],
            ) -> Result<SourceRangeReadOutcome, SourceRangeError>;
        }
    }

    pub(crate) const fn supports_reverse_source(&self) -> bool {
        self.supports_reverse_source
    }
}
