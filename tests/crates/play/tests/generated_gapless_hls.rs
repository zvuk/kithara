use kithara::{
    abr::AbrMode,
    audio::{AudioConfig, AudioControl, AudioRead, ReadOutcome},
    hls::{Hls, HlsConfig},
    platform::{
        CancelToken,
        time::{Duration, sleep},
    },
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    memory_asset_store,
};
use kithara_test_fixtures::signal::ascending_phase_replays;

struct Consts;

impl Consts {
    const BOUNDARY_SECONDS: [u64; 8] = [4, 8, 18, 28, 38, 48, 58, 68];
    const CHANNELS: usize = 2;
    const LOSSLESS_VARIANT: usize = 3;
    const PHASE_TOLERANCE: i32 = 3;
    const READ_SAMPLES: usize = 8_192;
    const SAMPLE_RATE: usize = 44_100;
    const WINDOW_FRAMES: usize = 2_048;
}

#[kithara::fixture]
async fn server() -> TestServerHelper {
    TestServerHelper::new().await
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(90)),
    hang_timeout_secs(2)
)]
#[case::plain("/assets/hls-gapless/master.m3u8")]
#[case::drm("/assets/drm-gapless/master.m3u8")]
async fn nonuniform_gapless_hls_is_continuous_at_every_boundary(
    #[case] path: &str,
    #[future(awt)] server: TestServerHelper,
) {
    let pools = pools();
    let hls = HlsConfig::for_url(server.url(path))
        .store(memory_asset_store())
        .pools(pools.clone())
        .cancel(CancelToken::never())
        .initial_abr_mode(AbrMode::manual(Consts::LOSSLESS_VARIANT))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls).build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let mut audio = worker
        .open(config)
        .await
        .unwrap_or_else(|error| panic!("open {path}: {error}"));
    audio
        .preload()
        .unwrap_or_else(|error| panic!("preload {path}: {error}"));
    assert_eq!(
        audio.spec().channels,
        u16::try_from(Consts::CHANNELS).expect("invariant: stereo")
    );
    assert_eq!(
        audio.spec().sample_rate.get(),
        u32::try_from(Consts::SAMPLE_RATE).expect("invariant: fixture rate fits u32")
    );

    let mut pcm = Vec::new();
    let mut block = vec![0.0; Consts::READ_SAMPLES];
    loop {
        match audio.read(&mut block) {
            Ok(ReadOutcome::Frames { count, .. }) => pcm.extend_from_slice(&block[..count.get()]),
            Ok(ReadOutcome::Pending { .. }) => sleep(Duration::from_millis(1)).await,
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(error) => panic!("read {path}: {error}"),
        }
    }

    assert!(pcm.len().is_multiple_of(Consts::CHANNELS));
    let left = pcm
        .chunks_exact(Consts::CHANNELS)
        .map(|frame| frame[0])
        .collect::<Vec<_>>();
    let whole = ascending_phase_replays(&left, 0, left.len(), Consts::PHASE_TOLERANCE);
    assert!(
        whole.is_empty(),
        "{path}: phase replay in full track: {whole:?}"
    );

    for seconds in Consts::BOUNDARY_SECONDS {
        let seconds = usize::try_from(seconds).expect("invariant: boundary fits usize");
        let boundary = seconds * Consts::SAMPLE_RATE;
        let start = boundary.saturating_sub(Consts::WINDOW_FRAMES);
        let end = boundary.saturating_add(Consts::WINDOW_FRAMES);
        assert!(
            end <= left.len(),
            "{path}: decoded track ends before {seconds}s boundary"
        );
        let replays = ascending_phase_replays(&left, start, end, Consts::PHASE_TOLERANCE);
        assert!(
            replays.is_empty(),
            "{path}: phase replay around {seconds}s boundary: {replays:?}"
        );
    }
}
