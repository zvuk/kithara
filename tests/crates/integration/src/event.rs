#[cfg(any(feature = "all", feature = "wasm"))]
use kithara::queue::{ItemEvent, QueueEvent};
use kithara::{
    abr::AbrEvent,
    assets::AssetEvent,
    audio::{AudioEvent, DecoderEvent},
    download::DownloaderEvent,
    file::FileEvent,
    hls::HlsEvent,
    host::TransportEvent,
    play::PlayerEvent,
};
use kithara_events::{BusEvent, EventSet};

/// Domains inspected by shared integration-test waits and event predicates.
#[derive(Clone, Debug, EventSet)]
#[non_exhaustive]
pub enum TestEvent {
    Abr(AbrEvent),
    Asset(AssetEvent),
    Audio(AudioEvent),
    Bus(BusEvent),
    Decoder(DecoderEvent),
    Downloader(DownloaderEvent),
    File(FileEvent),
    Hls(HlsEvent),
    #[cfg(any(feature = "all", feature = "wasm"))]
    Item(ItemEvent),
    Player(PlayerEvent),
    #[cfg(any(feature = "all", feature = "wasm"))]
    Queue(QueueEvent),
    Transport(TransportEvent),
}
