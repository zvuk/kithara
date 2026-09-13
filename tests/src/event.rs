use kithara::{
    abr::AbrEvent,
    assets::AssetEvent,
    audio::{AudioEvent, DecoderEvent},
    download::DownloaderEvent,
    file::FileEvent,
    hls::HlsEvent,
    host::TransportEvent,
    play::PlayerEvent,
    queue::{ItemEvent, QueueEvent},
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
    Item(ItemEvent),
    Player(PlayerEvent),
    Queue(QueueEvent),
    Transport(TransportEvent),
}
