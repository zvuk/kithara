use super::{DecoderNode, PcmPacket, PcmReceiver};

#[cfg(test)]
pub(crate) fn terminal_ring(
    blocking: bool,
    spec: kithara_signal::AudioSpec,
) -> (PcmReceiver, impl FnMut(PcmPacket) -> Result<(), PcmPacket>) {
    use ringbuf::traits::Producer;
    let (receiver, mut producer) = super::reader::packet_fixture(blocking, spec);
    (receiver, move |packet| {
        let result = producer.forward.try_push(packet);
        producer.signal();
        result
    })
}

#[cfg(test)]
pub(crate) fn terminal_node<T>(
    source: T,
    spec: kithara_signal::AudioSpec,
    blocking: bool,
) -> (
    DecoderNode<T, kithara_test_utils::bufpool::TestPools>,
    PcmReceiver,
    kithara_command::Sender<crate::LaneProtocol>,
)
where
    T: kithara_audio::AudioSource<Chunk = kithara_signal::AudioChunk>,
{
    let pools = crate::test_pools::pools();
    let (receiver, producer) = super::reader::packet_fixture(blocking, spec);
    let (sender, inbox) =
        kithara_command::channel(kithara_command::ChannelConfig::builder().build());
    let config = kithara_warp::WarpConfig::builder().build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let drain = kithara_effects::EffectDrain::new(0, &pools).expect("empty effect drain");
    let warp = crate::WarpSource::new(
        source,
        renderer,
        Vec::new(),
        drain,
        spec,
        pools.clone(),
        crate::LaneSetup {
            inbox,
            preload_chunks: std::num::NonZeroUsize::MIN,
            declick: crate::consts::DEFAULT_DECLICK,
        },
    );
    let node = DecoderNode::new(
        warp,
        producer,
        None,
        kithara_signal::AudioChunkInfo {
            spec,
            ..Default::default()
        },
        None,
        pools,
        None,
    );
    (node, receiver, sender)
}
