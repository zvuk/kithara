/// Restores an open pass from its resume record. A pass and its record exist
/// together or not at all; anything else is a corrupt archive.
pub(super) fn restore<P, R>(
    pass: Option<&mut P>,
    resume: Option<R>,
    apply: impl FnOnce(&mut P, R) -> Result<(), crate::BlobError>,
) -> Result<(), crate::BlobError> {
    match (pass, resume) {
        (Some(pass), Some(resume)) => apply(pass, resume),
        (None, None) => Ok(()),
        (Some(_), None) | (None, Some(_)) => Err(crate::BlobError::Corrupt),
    }
}
