mod entry;
#[cfg(test)]
pub(crate) mod fixtures;
mod handle;
mod run;
mod service;
#[cfg(test)]
mod tests;

pub(crate) use handle::AnalysisHandle;
#[cfg(test)]
pub(crate) use handle::Request;
pub(crate) use service::AnalysisService;
