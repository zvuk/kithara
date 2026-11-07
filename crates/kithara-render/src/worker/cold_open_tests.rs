use std::{future::poll_fn, task::Poll};

use kithara_assets::{AssetStore, StorageBackend};
use kithara_audio::{Audio, AudioConfig, NoResamplerBackend};
use kithara_command::{Batch, ChannelConfig, Inbox, Outcome, Sender, When, channel};
use kithara_events::EventBus;
use kithara_file::{File, FileConfig, FileSrc};
use kithara_platform::{
    sync::{Arc, ThreadGate, WaitGate},
    time::{Duration, timeout},
    tokio::{sync::oneshot, task::yield_now},
};
use kithara_stream::{SourceError, Stream, StreamType};
use kithara_sync::SpeedCurve;
use kithara_test_fixtures::signal::{Wave, wav};
use kithara_test_utils::{TestTempDir, kithara};
use kithara_warp::StretchKind;

use super::super::{DecoderNode, PcmPacket, PcmReceiver, PlayWorker, PlayWorkerConfig};
use crate::{
    LaneProtocol, ServiceClass,
    dispatcher::{
        Dispatched, DispatcherCommand, DispatcherProtocol, LaneStart, LoadRequest, Open, OpenResult,
    },
    test_pools::{TestPools, pools},
};

struct Gate {
    started: oneshot::Sender<()>,
    release: Arc<ThreadGate>,
}

struct GatedFile;

impl StreamType for GatedFile {
    type Config = (FileConfig<TestPools>, Option<Gate>);
    type Events = EventBus;
    type Source = <File<TestPools> as StreamType>::Source;

    async fn create((file, gate): Self::Config) -> Result<Self::Source, SourceError> {
        if let Some(gate) = gate {
            let since = gate.release.current();
            let _ = gate.started.send(());
            let _io = kithara_platform::flash::real_io();
            assert!(
                gate.release.wait_timeout(since, Duration::from_secs(5)),
                "cold open released"
            );
        }
        File::<TestPools>::create(file).await
    }
}

struct OpeningFile {
    worker: PlayWorker<TestPools>,
    config: AudioConfig<GatedFile, NoResamplerBackend>,
}

impl std::fmt::Debug for OpeningFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpeningFile")
            .finish_non_exhaustive()
    }
}

impl Open for OpeningFile {
    type Opened = PcmReceiver;
    type Lane = DecoderNode<Audio<Stream<GatedFile>>, TestPools>;

    async fn open(
        self,
        position: Duration,
        start: LaneStart,
        inbox: Inbox<LaneProtocol>,
    ) -> OpenResult<Self::Opened, Self::Lane> {
        self.worker.load(self.config, position, start, inbox).await
    }
}

type Loads = DispatcherProtocol<OpeningFile>;

fn load(
    sender: &mut Sender<Loads>,
    item: OpeningFile,
    class: ServiceClass,
) -> Sender<LaneProtocol> {
    let (commands, inbox) = channel(ChannelConfig::builder().build());
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                    item,
                    class,
                    position: Duration::ZERO,
                    start: LaneStart {
                        speed: SpeedCurve::Constant(1.0),
                        keylock: false,
                        backend: StretchKind::default(),
                    },
                    inbox,
                }))],
            },
        )
        .expect("source load admitted");
    commands
}

async fn loaded(sender: &mut Sender<Loads>) -> PcmReceiver {
    let receipt = poll_fn(|cx| {
        sender.hold(cx.waker().clone());
        sender.receipts().next().map_or(Poll::Pending, Poll::Ready)
    })
    .await;
    sender.release();
    let (outcome, _) = receipt.into();
    let Outcome::Applied {
        data: Dispatched::Loaded(loaded),
        ..
    } = outcome
    else {
        panic!("source opened: {outcome:?}")
    };
    loaded.opened
}

#[kithara::test(tokio)]
async fn a_slow_background_open_keeps_an_audible_lane_producing() {
    let directory = TestTempDir::new();
    let path = directory.path().join("audible.wav");
    std::fs::write(&path, wav(44_100, 2, 441_000, Wave::sine(440.0))).expect("write source");
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let file = FileConfig::for_src(FileSrc::Local(path))
        .store(
            AssetStore::builder(pools)
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let config = |gate| {
        AudioConfig::<GatedFile, NoResamplerBackend>::for_stream((file.clone(), gate)).build()
    };
    let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let _dispatcher = worker.start_dispatcher(inbox).expect("playback dispatcher");
    let _audible_commands = load(
        &mut sender,
        OpeningFile {
            worker: worker.clone(),
            config: config(None),
        },
        ServiceClass::Audible,
    );
    let mut pcm = loaded(&mut sender).await;
    pcm.set_playing(true);
    let (started, received) = oneshot::channel();
    let release = Arc::new(ThreadGate::default());
    let _background_commands = load(
        &mut sender,
        OpeningFile {
            worker: worker.clone(),
            config: config(Some(Gate {
                started,
                release: Arc::clone(&release),
            })),
        },
        ServiceClass::Idle,
    );
    received.await.expect("background open started");
    // More than a full default ring proves replenishment during the blocked open.
    let progress = timeout(Duration::from_secs(1), async {
        let mut frame = None;
        for _ in 0..24 {
            let packet = loop {
                if let Some(packet) = pcm.pop() {
                    break packet;
                }
                yield_now().await;
            };
            let PcmPacket::Chunk(chunk) = &packet else {
                panic!("audible source failed")
            };
            assert!(chunk.samples.iter().any(|sample| sample.abs() > 0.01));
            if let Some(previous) = frame {
                assert_eq!(chunk.meta.lane_frame, previous);
            }
            frame = Some(chunk.meta.lane_frame + u64::from(chunk.meta.frames));
            assert_eq!(chunk.meta.segment, kithara_signal::SegmentId::FIRST);
            pcm.recycle(packet).expect("recycle consumed PCM");
        }
    })
    .await;
    release.signal();
    progress.expect("the Audible lane must refill while another source blocks in cold open");
    let _background = loaded(&mut sender).await;
}
