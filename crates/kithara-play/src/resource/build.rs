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
        let audio_buffer_chunks = self.audio_buffer_chunks.max(self.preload_chunks).get();
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
        let extension = self.hint.clone().or(derived_hint);
        let downloader = self.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = CancelScope::new(self.cancel.clone()).token();
            let client = HttpClient::new(NetOptions::default(), pools.clone(), dl_cancel.child());
            Downloader::new(
                DownloaderConfig::for_client(client)
                    .cancel(dl_cancel)
                    .build(),
            )
        });
        let file_config = FileConfig::for_src(file_src)
            .store(self.store.clone())
            .downloader(downloader)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers.clone())
            .maybe_discriminator(self.discriminator.clone())
            .maybe_extension(extension.clone())
            .pools(pools)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .build();
        AudioConfig::<kithara_file::File<S>, B>::for_stream(file_config)
            .audio_buffer_chunks(audio_buffer_chunks)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(extension)
            .maybe_host_sample_rate(self.host_sample_rate)
            .maybe_observer(observer)
            .preload_chunks(self.preload_chunks)
            .decoder(self.decoder)
            .consumer_wake_mode(
                self.consumer_wake_mode
                    .unwrap_or(ConsumerWakeMode::ImmediateOffRt),
            )
            .block_on_underrun(self.block_on_underrun)
            .build()
    }

    /// Build an `AudioConfig<Hls<S>>` from this resource configuration.
    pub(crate) fn build_hls_config(
        self,
        worker: &PlayWorker<S>,
        observer: Option<Box<dyn AudioObserver>>,
    ) -> Result<AudioConfig<kithara_hls::Hls<S>, B>, DecodeError> {
        let pools = worker.pools().clone();
        let audio_buffer_chunks = self.audio_buffer_chunks.max(self.preload_chunks).get();
        let url = match self.src {
            ResourceSrc::Url(ref url) => url.clone(),
            ResourceSrc::Path(_) => {
                return Err(DecodeError::InvalidData {
                    detail: "HLS requires a URL, got a local path",
                });
            }
        };
        let hls_config = HlsConfig::for_url(url)
            .store(self.store.clone())
            .keys(self.keys)
            .maybe_downloader(self.downloader)
            .initial_abr_mode(self.initial_abr_mode)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers)
            .maybe_discriminator(self.discriminator)
            .maybe_base_url(self.hls_base_url)
            .pools(pools)
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .size_probe_method(self.size_probe_method)
            .build();
        Ok(
            AudioConfig::<kithara_hls::Hls<S>, B>::for_stream(hls_config)
                .audio_buffer_chunks(audio_buffer_chunks)
                .maybe_cancel(self.cancel.clone())
                .maybe_hint(self.hint)
                .maybe_host_sample_rate(self.host_sample_rate)
                .maybe_observer(observer)
                .preload_chunks(self.preload_chunks)
                .decoder(self.decoder)
                .consumer_wake_mode(
                    self.consumer_wake_mode
                        .unwrap_or(ConsumerWakeMode::ImmediateOffRt),
                )
                .block_on_underrun(self.block_on_underrun)
                .build(),
        )
    }
}
