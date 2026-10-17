use crate::{ElasticError, ElasticRequest};

pub(crate) const fn with_output_source_frames(
    mut request: ElasticRequest,
    frames: usize,
) -> Result<ElasticRequest, ElasticError> {
    if frames == 0 {
        return Err(ElasticError::EmptySource);
    }
    request.output_source_frames = frames;
    Ok(request)
}
