use std::{num::NonZeroUsize, path::absolute};

use kithara::{
    analysis::{
        AnalysisDemand, AnalysisOutcome, AnalysisToken, AnalysisWorker, AnalysisWorkerConfig,
        AnalyzerBuilder,
    },
    assets::{AssetStore, StorageBackend},
    download::{Downloader, DownloaderConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken, thread,
        tokio::{runtime::Handle, select},
    },
    play::{PlaybackResamplerBackend, Resource, ResourceConfig, ResourceSrc},
};

use crate::{
    RunError,
    input::{Args, Artifact, Track},
    pools::{self, AnalyzePools, Pools},
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct OpenError(String);

pub(crate) struct Engine {
    worker: AnalysisWorker,
    store: AssetStore<AnalyzePools>,
    cancel: CancelToken,
    downloader: Downloader,
    pools: Pools,
}

impl Engine {
    pub(crate) async fn analyze(&self, track: &Track) -> Result<AnalysisOutcome, OpenError> {
        let resource = select! {
            biased;
            () = self.cancel.cancelled() => return Ok(AnalysisOutcome::Cancelled { last: None }),
            result = self.open(track) => result,
        };
        if self.cancel.is_cancelled() {
            return Ok(AnalysisOutcome::Cancelled { last: None });
        }
        let resource = resource?;
        let rate = resource.spec().sample_rate;
        let (results, producer, completion) = self
            .worker
            .analyze(
                resource.into(),
                AnalysisToken::from(track.id.as_str()),
                rate,
                0,
                AnalysisDemand::ALL,
            )
            .map_err(|error| OpenError(error.to_string()))?;
        drop(results);
        drop(producer);
        Ok(completion.outcome().await)
    }

    async fn open(&self, track: &Track) -> Result<Resource, OpenError> {
        let src = if track.src.starts_with("http://") || track.src.starts_with("https://") {
            ResourceSrc::parse(&track.src).map_err(|error| OpenError(error.to_string()))?
        } else {
            ResourceSrc::Path(absolute(&track.src).map_err(|error| OpenError(error.to_string()))?)
        };
        let config = ResourceConfig::<AnalyzePools>::for_src(src)
            .store(self.store.clone())
            .downloader(self.downloader.clone())
            .cancel(self.cancel.child())
            .build();
        let mut resource = Resource::open(config, self.worker.wake_handle(), self.pools.clone())
            .await
            .map_err(|error| OpenError(error.to_string()))?;
        resource
            .preload()
            .await
            .map_err(|error| OpenError(error.to_string()))?;
        Ok(resource)
    }

    pub(crate) fn try_new(args: &Args, cancel: CancelToken) -> Result<Self, RunError> {
        let parallelism = thread::available_parallelism()
            .ok_or_else(|| std::io::Error::other("processor count unavailable"))?;
        let pools = pools::build(args.jobs.unwrap_or(parallelism))?;
        let mut builder =
            AnalyzerBuilder::<PlaybackResamplerBackend, AnalyzePools>::new(pools.clone());
        for artifact in &args.artifacts {
            builder = match artifact {
                Artifact::Waveform => builder.with_waveform(args.waveform_fold),
                Artifact::Beat => builder.with_beat(),
                Artifact::Key => builder.with_key(),
            };
        }
        let worker = AnalysisWorker::new(
            AnalysisWorkerConfig::for_builder(builder)
                .max_compute_tasks(parallelism)
                .cancel(cancel.clone())
                .build(),
        );
        let store = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .build();
        let client = HttpClient::new(NetOptions::default(), pools.clone(), cancel.child());
        let downloader = Downloader::new(
            DownloaderConfig::for_client(client)
                .runtime(Handle::current())
                .cancel(cancel.child())
                .build(),
        );
        Ok(Self {
            worker,
            store,
            cancel,
            downloader,
            pools,
        })
    }
}

pub(crate) fn jobs(args: &Args) -> Result<NonZeroUsize, RunError> {
    Ok(match args.jobs {
        Some(jobs) => jobs,
        None => thread::available_parallelism()
            .ok_or_else(|| std::io::Error::other("processor count unavailable"))?,
    })
}
