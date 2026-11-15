//! A deck renders full lane packets whatever its decoder's chunking: a packet
//! ends early only at a cut (a command, a discontinuity, the end of the track).

use kithara_bufpool::PoolRegion;

use super::{doubles::*, *};

/// The lane packet: the render quantum the fixtures configure.
const PACKET: u32 = 512;

/// FDK AAC's output delay splits each 1024-frame AAC frame into these chunks.
const SHORT: u32 = 363;
const LONG: u32 = 661;

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"))
}

/// Source frames `frame..frame + frames` as a stereo ramp: every sample holds
/// its own source frame, so a gap or a duplicate shows in the samples.
fn ramp(pools: &PoolRegion<TestPools>, frame: u64, frames: u32) -> AudioChunk {
    let samples = (frame..frame + u64::from(frames))
        .flat_map(|frame| {
            let value: f32 = frame.as_();
            [value, value]
        })
        .collect::<Vec<_>>();
    chunk_with_frames(pools, spec(), frame, frames, &samples)
}

/// Contiguous chunks of `sizes` frames from source frame 0.
fn chunks(pools: &PoolRegion<TestPools>, sizes: &[u32]) -> VecDeque<AudioChunk> {
    let mut frame = 0;
    sizes
        .iter()
        .map(|&frames| {
            let chunk = ramp(pools, frame, frames);
            frame += u64::from(frames);
            chunk
        })
        .collect()
}

/// A unity lane with the default backend over `source`.
fn packet_lane<T>(
    pools: &PoolRegion<TestPools>,
    source: T,
) -> (
    WarpSource<T, TestPools>,
    kithara_command::Sender<LaneProtocol>,
)
where
    T: AudioSource<Chunk = AudioChunk>,
{
    let (lane, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
    let config = kithara_warp::WarpConfig::builder()
        .speed(1.0)
        .render_quantum_frames(NonZeroUsize::new(PACKET as usize).expect("quantum"))
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec(), pools.clone());
    let source = WarpSource::new(
        source,
        renderer,
        spec(),
        pools.clone(),
        LaneSetup {
            inbox,
            preload_chunks: NonZeroUsize::new(1).expect("preload"),
            declick: consts::DEFAULT_DECLICK,
        },
    );
    (source, lane)
}

/// Every packet the lane emits, as `(source start, frames)`, until it reports
/// EOF or blocks `blocked_limit` times in a row. Checks that each packet's
/// samples are the ramp from its source start.
#[kithara::hang_watchdog]
fn packets<T>(source: &mut WarpSource<T, TestPools>, blocked_limit: usize) -> Vec<(u64, u32)>
where
    T: AudioSource<Chunk = AudioChunk>,
{
    let mut packets = Vec::new();
    let mut blocked = 0;
    for _ in 0..4096 {
        hang_tick!();
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                blocked = 0;
                let start = data.meta.frame_offset;
                for (frame, source_frame) in data.samples.chunks(2).zip(start..) {
                    let expected: f32 = source_frame.as_();
                    assert_eq!(
                        frame,
                        [expected, expected],
                        "packet from source frame {start} skips or repeats at {source_frame}"
                    );
                }
                packets.push((start, data.meta.frames));
                hang_reset!();
            }
            TrackStep::Produced(_) | TrackStep::StateChanged => {}
            TrackStep::Blocked(_) => {
                blocked += 1;
                if blocked == blocked_limit {
                    return packets;
                }
            }
            TrackStep::Eof => return packets,
            TrackStep::Failed(failure) => panic!("the lane failed: {failure:?}"),
        }
        flush_deferred(source);
    }
    panic!("the lane never settled: {packets:?}");
}

fn sizes(packets: &[(u64, u32)]) -> Vec<u32> {
    packets.iter().map(|&(_, frames)| frames).collect()
}

#[kithara::test]
fn short_and_long_decoder_chunks_render_as_full_packets() {
    let pools = pools();
    let raw = RawSource {
        chunks: chunks(&pools, &[SHORT, LONG, SHORT, LONG, SHORT, LONG]),
        head: Arc::new(AtomicU64::new(0)),
    };
    let (mut source, _lane) = packet_lane(&pools, raw);

    let packets = packets(&mut source, usize::MAX);

    assert_eq!(sizes(&packets), vec![PACKET; 6]);
    assert_eq!(
        packets.last().map(|&(start, _)| start),
        Some(5 * u64::from(PACKET))
    );
}

#[kithara::test]
fn the_track_end_cuts_the_last_packet() {
    let pools = pools();
    let raw = RawSource {
        chunks: chunks(&pools, &[SHORT, LONG, 200]),
        head: Arc::new(AtomicU64::new(0)),
    };
    let (mut source, _lane) = packet_lane(&pools, raw);

    assert_eq!(
        sizes(&packets(&mut source, usize::MAX)),
        [PACKET, PACKET, 200]
    );
}

#[kithara::test]
fn a_command_cuts_the_packet_at_its_frame() {
    const AT: u64 = 100;
    let pools = pools();
    let raw = RawSource {
        chunks: chunks(&pools, &[SHORT, LONG, SHORT, LONG]),
        head: Arc::new(AtomicU64::new(0)),
    };
    let (mut source, mut lane) = packet_lane(&pools, raw);
    lane.send(
        When::At(LaneFrame {
            segment: SegmentId::FIRST,
            frame: AT,
        }),
        command_batch(LaneCommand::SetSpeed(SpeedCurve::Constant(1.0))),
    )
    .expect("the lane has room for one batch");

    let packets = packets(&mut source, usize::MAX);

    assert_eq!(packets.first(), Some(&(0, 100)));
    assert_eq!(packets.get(1).map(|&(start, _)| start), Some(AT));
    assert_eq!(sizes(&packets), [100, PACKET, PACKET, PACKET, 412]);
}

#[kithara::test]
fn a_stalled_source_holds_staged_frames_for_the_next_packet() {
    let pools = pools();
    let mut all = chunks(&pools, &[SHORT, LONG, SHORT, LONG]);
    let first = all.pop_front().expect("first chunk");
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(1, spec())));
    let raw = RevisionSource {
        chunks: VecDeque::from([first]),
        discontinuity,
    };
    let (mut source, _lane) = packet_lane(&pools, raw);

    let held = packets(&mut source, 4);
    assert_eq!(
        held,
        [],
        "a stall is not a cut: the {SHORT} staged frames wait"
    );

    source.source.chunks.extend(all);
    let packets = packets(&mut source, 4);

    assert_eq!(sizes(&packets), vec![PACKET; 4]);
    assert_eq!(packets.first().map(|&(start, _)| start), Some(0));
}

#[kithara::test]
fn a_discontinuity_never_joins_staged_frames_to_the_next_chunk() {
    let pools = pools();
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(1, spec())));
    let raw = RevisionSource {
        chunks: chunks(&pools, &[SHORT]),
        discontinuity: Arc::clone(&discontinuity),
    };
    let (mut source, _lane) = packet_lane(&pools, raw);
    assert_eq!(packets(&mut source, 4), []);

    *discontinuity.lock() = SourceDiscontinuity::new(2, spec());
    source.source.chunks.push_back(ramp(&pools, 10_000, PACKET));
    let packets = packets(&mut source, 4);

    assert_eq!(packets, [(0, SHORT), (10_000, PACKET)]);
}

/// An ABR variant promotion keeps the raw source coordinates but may drop the
/// generation's visible origin. Frames on both sides still join into one
/// packet, which carries the origin of the chunk it ends in.
#[kithara::test]
fn a_variant_origin_change_joins_contiguous_frames() {
    let pools = pools();
    let mut chunks = chunks(&pools, &[SHORT, LONG]);
    if let Some(outgoing) = chunks.front_mut() {
        outgoing.meta.visible_origin = Some(Duration::ZERO);
    }
    let raw = RawSource {
        chunks,
        head: Arc::new(AtomicU64::new(0)),
    };
    let (mut source, _lane) = packet_lane(&pools, raw);

    let first = (0..64)
        .find_map(|_| match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => Some(data.meta),
            TrackStep::Failed(failure) => panic!("the lane failed: {failure:?}"),
            TrackStep::Eof | TrackStep::Blocked(_) => panic!("the lane stopped before a packet"),
            TrackStep::Produced(_) | TrackStep::StateChanged => {
                flush_deferred(&mut source);
                None
            }
        })
        .expect("the lane renders a packet");

    assert_eq!((first.frame_offset, first.frames), (0, PACKET));
    assert_eq!(first.visible_origin, None);
}
