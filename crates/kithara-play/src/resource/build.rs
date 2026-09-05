use kithara_audio::{AudioConfig, AudioObserver, ConsumerWakeMode, ResamplerBackend};
use kithara_bufpool::HasPool;
use kithara_decode::DecodeError;
use kithara_file::{FileConfig, FileSrc};
use kithara_hls::HlsConfig;
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::CancelScope;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use url::Url;

use super::{ResourceConfig, ResourceSrc};
use crate::PlayWorker;

fn derive_remote_file_hint(url: &Url) -> Option<String> {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(derive_extension_hint)
}

fn derive_extension_hint(segment: &str) -> Option<String> {
    let (_, extension) = segment.rsplit_once('.')?;
    if extension.is_empty() || !extension.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    Some(extension.to_lowercase())
}

impl<S, B> ResourceConfig<S, B>
where
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Build an `AudioConfig<File<S>>` from this resource configuration.
    pub(crate) fn build_file_config(
        self,
        worker: &PlayWorker<S>,
        observer: Option<Box<dyn AudioObserver>>,
    ) -> AudioConfig<kithara_file::File<S>, B> {
        let pools = worker.pools().clone();
        let (file_src, derived_hint) = match self.src {
            ResourceSrc::Url(ref url) => {
                (FileSrc::Remote(url.clone()), derive_remote_file_hint(url))
            }
            ResourceSrc::Path(ref path) => (
                FileSrc::Local(path.clone()),
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase),
            ),
        };
        let named_extension = self.hint.clone().or(derived_hint);
        let downloader = self.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = CancelScope::new(self.cancel.clone()).token();
            let client = HttpClient::new(NetOptions::default(), pools.clone(), dl_cancel.child());
            Downloader::new(
                DownloaderConfig::for_client(client)
                    .cancel(dl_cancel)
                    .build(),
            )
        });
        let mut file_config = FileConfig::for_src(file_src)
            .store(self.store.clone())
            .downloader(downloader)
            .maybe_headers(self.headers.clone())
            .maybe_discriminator(self.discriminator.clone())
            .pools(pools)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .build();
        file_config.apply(self.file.clone());
        // The hint the caller passed and the one derived from the source both
        // describe this very track, so either outranks a document's blanket
        // `file.extension`.
        let extension = named_extension.or_else(|| file_config.extension.clone());
        file_config.extension = extension.clone();
        let mut audio_config = AudioConfig::<kithara_file::File<S>, B>::for_stream(file_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(extension)
            .maybe_observer(observer)
            .consumer_wake_mode(
                self.consumer_wake_mode
                    .unwrap_or(ConsumerWakeMode::ImmediateOffRt),
            )
            .block_on_underrun(self.block_on_underrun)
            .maybe_host_sample_rate(self.host_sample_rate)
            .decoder(self.decoder)
            .build();
        audio_config.apply(self.audio.clone());
        audio_config
    }

    /// Build an `AudioConfig<Hls<S>>` from this resource configuration.
    pub(crate) fn build_hls_config(
        self,
        worker: &PlayWorker<S>,
        observer: Option<Box<dyn AudioObserver>>,
    ) -> Result<AudioConfig<kithara_hls::Hls<S>, B>, DecodeError> {
        let pools = worker.pools().clone();
        let url = match self.src {
            ResourceSrc::Url(ref url) => url.clone(),
            ResourceSrc::Path(_) => {
                return Err(DecodeError::InvalidData {
                    detail: "HLS requires a URL, got a local path",
                });
            }
        };
        let mut hls_config = HlsConfig::for_url(url)
            .store(self.store.clone())
            .keys(self.keys)
            .maybe_downloader(self.downloader)
            .initial_abr_mode(self.initial_abr_mode)
            .maybe_headers(self.headers)
            .maybe_discriminator(self.discriminator)
            .maybe_base_url(self.hls_base_url)
            .pools(pools)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .build();
        hls_config.apply(self.hls.clone());
        let mut audio_config = AudioConfig::<kithara_hls::Hls<S>, B>::for_stream(hls_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint)
            .maybe_observer(observer)
            .consumer_wake_mode(
                self.consumer_wake_mode
                    .unwrap_or(ConsumerWakeMode::ImmediateOffRt),
            )
            .block_on_underrun(self.block_on_underrun)
            .maybe_host_sample_rate(self.host_sample_rate)
            .decoder(self.decoder)
            .build();
        audio_config.apply(self.audio.clone());
        Ok(audio_config)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_assets::AssetStore;
    use kithara_audio::AudioConfigPatch;
    use kithara_test_utils::kithara;

    use crate::{
        PlayWorker, PlayWorkerConfig,
        resource::{ResourceConfig, ResourceSrc},
        test_pools::{TestPools, pools},
    };

    fn worker() -> PlayWorker<TestPools> {
        PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
    }

    fn config(source: &str) -> ResourceConfig<TestPools> {
        ResourceConfig::for_src(ResourceSrc::parse(source).expect("valid test source"))
            .store(AssetStore::builder(pools()).build())
            .build()
    }

    /// `download_batch_size` reached the built stream through no path while
    /// `ResourceConfig` re-declared HLS knobs one by one: it never mirrored
    /// this one, so a caller going through `PlayerConfig` could not set it at
    /// all. Carrying the HLS patch whole closes that gap for every knob the
    /// crate declares, now and later.
    #[kithara::test]
    fn an_hls_knob_the_resource_never_declared_reaches_the_built_config() {
        let mut config = config("https://example.com/live.m3u8");
        config.hls.download_batch_size = Some(6);

        let built = config
            .build_hls_config(&worker(), None)
            .expect("valid HLS config");

        assert_eq!(built.stream().download_batch_size, 6);
    }

    /// The same for the file branch: `reader_event_capacity` is a
    /// `kithara-file` knob `ResourceConfig` never mirrored.
    #[kithara::test]
    fn a_file_knob_the_resource_never_declared_reaches_the_built_config() {
        let mut config = config("https://example.com/song.mp3");
        config.file.reader_event_capacity = Some(512);

        let built = config.build_file_config(&worker(), None);

        assert_eq!(built.stream().reader_event_capacity, 512);
    }

    /// The per-call `hint` still lands as the file source's extension: it is
    /// per-call input, read by the decoder as well, so it stays on
    /// `ResourceConfig` and is mapped into `FileConfig::extension` once here.
    #[kithara::test]
    fn the_per_call_hint_becomes_the_file_extension() {
        let mut config = config("https://example.com/track/stream");
        config.hint = Some("flac".to_owned());

        let built = config.build_file_config(&worker(), None);

        assert_eq!(built.stream().extension.as_deref(), Some("flac"));
        assert_eq!(built.hint(), Some("flac"));
    }

    /// A document-named `extension` backs the per-call hint rather than being
    /// dropped: nothing more specific names one for a URL that carries no
    /// extension and no caller hint.
    #[kithara::test]
    fn a_document_extension_stands_when_nothing_more_specific_names_one() {
        let mut config = config("https://example.com/track/stream");
        config.file.extension = Some("wav".to_owned());

        let built = config.build_file_config(&worker(), None);

        assert_eq!(built.stream().extension.as_deref(), Some("wav"));
        assert_eq!(built.hint(), Some("wav"));
    }

    /// The per-call hint describes this very track, so it outranks a
    /// document's blanket `file.extension` rather than losing to whichever
    /// merge ran last.
    #[kithara::test]
    fn the_per_call_hint_outranks_a_document_extension() {
        let mut config = config("https://example.com/track/stream");
        config.hint = Some("flac".to_owned());
        config.file.extension = Some("wav".to_owned());

        let built = config.build_file_config(&worker(), None);

        assert_eq!(built.stream().extension.as_deref(), Some("flac"));
        assert_eq!(built.hint(), Some("flac"));
    }

    fn preload_chunks(count: usize) -> AudioConfigPatch {
        let mut patch = AudioConfigPatch::default();
        patch.preload_chunks = Some(NonZeroUsize::new(count).expect("a preload count above zero"));
        patch
    }

    /// `preload_chunks` is both a document key and read in production, so the
    /// value a document names has to survive the whole way to the built HLS
    /// pipeline.
    #[kithara::test]
    fn the_document_preload_count_reaches_the_built_hls_config() {
        let mut config = config("https://example.com/live.m3u8");
        config.audio = preload_chunks(9);

        let built = config
            .build_hls_config(&worker(), None)
            .expect("valid HLS config");

        assert_eq!(built.preload_chunks().get(), 9);
    }

    /// The file branch reads the same field from the same place.
    #[kithara::test]
    fn the_document_preload_count_reaches_the_built_file_config() {
        let mut config = config("https://example.com/song.mp3");
        config.audio = preload_chunks(9);

        let built = config.build_file_config(&worker(), None);

        assert_eq!(built.preload_chunks().get(), 9);
    }

    /// `audio_buffer_chunks` reached the built `AudioConfig` through no path
    /// while `ResourceConfig` forwarded individual audio fields one by one:
    /// it never mirrored this one. Carrying the audio patch whole closes that
    /// gap, the same collapse `hls` and `file` already went through.
    #[kithara::test]
    fn an_audio_knob_the_resource_never_declared_reaches_the_built_config() {
        let mut config = config("https://example.com/live.m3u8");
        config.audio.audio_buffer_chunks = Some(24);

        let built = config
            .build_hls_config(&worker(), None)
            .expect("valid HLS config");

        assert_eq!(built.audio_buffer_chunks(), 24);
    }
}
