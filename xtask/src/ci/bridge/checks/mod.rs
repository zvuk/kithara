mod github;
mod gitlab;
mod projection;

pub(super) use projection::{SourceCi, report_pull, review_size};
