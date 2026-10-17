use kithara_assets::{AssetStore, StorageBackend};
use kithara_file::{File, FileConfig, FileSrc};
use kithara_platform::{sync::Arc, tokio::task::yield_runnable};
use kithara_resampler::NoResamplerBackend;
use kithara_signal::AudioChunk;
use kithara_stream::{Stream, mock::NoopWorkerWake};
use kithara_test_fixtures::assets;

use crate::{
    Audio, AudioConfig,
    test_pools::{TestPools, pools},
};

pub(crate) async fn prepared_audio() -> Audio<Stream<File<TestPools>>> {
    let pools = pools();
    let path = assets::audio_wav_frames_44100()
        .path()
        .expect("native WAV fixture");
    let stream = FileConfig::for_src(FileSrc::Local(path.to_owned()))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(stream).build();
    Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
        .await
        .expect("prepare decoded source")
}

#[kithara_test_utils::kithara::hang_watchdog]
#[kithara_test_utils::kithara::flash(io)]
pub(crate) async fn produced_audio(audio: &mut Audio<Stream<File<TestPools>>>) -> AudioChunk {
    use crate::{AudioSource, Fetch, TrackStep};

    loop {
        hang_tick!();
        let _ = audio.prepare_deferred();
        match audio.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                hang_reset!();
                return data;
            }
            TrackStep::StateChanged => {
                hang_reset!();
            }
            TrackStep::Blocked(_) => {}
            _ => panic!("expected decoded output"),
        }
        audio.finish_deferred();
        yield_runnable().await;
    }
}
