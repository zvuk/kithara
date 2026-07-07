use std::sync::Arc;

use kithara_assets::{FlushHub, StoreOptions};
use kithara_audio::AudioConfig;
use kithara_decode::DecodeError;
use kithara_file::{FileConfig, FileSrc};
use kithara_hls::HlsConfig;
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::CancelScope;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use url::Url;

use super::{ResourceConfig, ResourceSrc};

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

fn store_options_with_flush_hub(
    store: &StoreOptions,
    flush_hub: Option<Arc<FlushHub>>,
) -> StoreOptions {
    StoreOptions::builder()
        .backend(store.backend.clone())
        .maybe_cache_capacity(store.cache_capacity)
        .maybe_flush_hub(flush_hub.or_else(|| store.flush_hub.clone()))
        .maybe_layout(store.layout.clone())
        .maybe_max_assets(store.max_assets)
        .maybe_max_bytes(store.max_bytes)
        .build()
}

impl ResourceConfig {
    /// Build an `AudioConfig<File>` from this resource configuration.
    pub(crate) fn build_file_config(self) -> AudioConfig<kithara_file::File> {
        let byte_pool = self.byte_pool.clone();
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
        let store = store_options_with_flush_hub(&self.store, self.flush_hub.clone());
        let downloader = self.downloader.clone().unwrap_or_else(|| {
            let dl_cancel = CancelScope::new(self.cancel.clone()).token();
            let net_options = NetOptions::builder()
                .maybe_byte_pool(byte_pool.clone())
                .build();
            let client = HttpClient::new(net_options, dl_cancel.child());
            Downloader::new(
                DownloaderConfig::for_client(client)
                    .cancel(dl_cancel)
                    .build(),
            )
        });
        let file_config = FileConfig::for_src(file_src)
            .store(store)
            .downloader(downloader)
            .maybe_asset_store(self.asset_store.clone())
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers.clone())
            .maybe_name(self.name.clone())
            .maybe_pool(byte_pool.clone())
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .build();
        AudioConfig::<kithara_file::File>::for_stream(file_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint.or(derived_hint))
            .maybe_byte_pool(byte_pool)
            .maybe_pcm_pool(self.pcm_pool)
            .maybe_host_sample_rate(self.host_sample_rate)
            .resampler_quality(self.resampler_quality)
            .preload_chunks(self.preload_chunks)
            .decoder_backend(self.decoder_backend)
            .maybe_playback_rate(self.playback_rate)
            .maybe_stretch(self.stretch)
            .maybe_engine_load(self.engine_load)
            .maybe_worker(self.worker)
            .gapless_mode(self.gapless_mode)
            .build()
    }

    /// Build an `AudioConfig<Hls>` from this resource configuration.
    pub(crate) fn build_hls_config(self) -> Result<AudioConfig<kithara_hls::Hls>, DecodeError> {
        let byte_pool = self.byte_pool.clone();
        let url = match self.src {
            ResourceSrc::Url(ref url) => url.clone(),
            ResourceSrc::Path(_) => {
                return Err(DecodeError::InvalidData {
                    detail: "HLS requires a URL, got a local path",
                });
            }
        };
        let store = store_options_with_flush_hub(&self.store, self.flush_hub.clone());
        let hls_config = HlsConfig::for_url(url)
            .store(store)
            .maybe_asset_store(self.asset_store)
            .keys(self.keys)
            .maybe_downloader(self.downloader)
            .initial_abr_mode(self.initial_abr_mode)
            .maybe_look_ahead_bytes(self.look_ahead_bytes)
            .maybe_headers(self.headers)
            .maybe_name(self.name)
            .maybe_base_url(self.hls_base_url)
            .maybe_pool(byte_pool.clone())
            .maybe_events(self.bus.clone())
            .maybe_cancel(self.cancel.clone())
            .size_probe_method(self.size_probe_method)
            .build();
        Ok(AudioConfig::<kithara_hls::Hls>::for_stream(hls_config)
            .maybe_cancel(self.cancel.clone())
            .maybe_hint(self.hint)
            .maybe_byte_pool(byte_pool)
            .maybe_pcm_pool(self.pcm_pool)
            .maybe_host_sample_rate(self.host_sample_rate)
            .resampler_quality(self.resampler_quality)
            .preload_chunks(self.preload_chunks)
            .decoder_backend(self.decoder_backend)
            .maybe_playback_rate(self.playback_rate)
            .maybe_stretch(self.stretch)
            .maybe_engine_load(self.engine_load)
            .maybe_worker(self.worker)
            .gapless_mode(self.gapless_mode)
            .build())
    }
}
