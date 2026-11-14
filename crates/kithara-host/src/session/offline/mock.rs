use crate::session::context::SessionGraph;
pub(crate) fn start(
    context: &mut SessionGraph,
    sample_rate: u32,
    decks: &mut [crate::session::state::DeckNode],
) -> Result<crate::session::state::SessionStream, String> {
    let config = super::backend::BackendConfig::builder()
        .sample_rate(std::num::NonZeroU32::new(sample_rate).ok_or("zero sample rate")?)
        .block_frames(std::num::NonZeroU32::new(512).ok_or("zero block size")?)
        .declared_latency(kithara_platform::time::Duration::ZERO)
        .build();
    super::backend::OfflineStream::start(context, config, decks)
        .map(|stream| crate::session::state::SessionStream::Offline(Box::new(stream)))
        .map_err(|error| error.to_string())
}
