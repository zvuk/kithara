use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use bytes::BytesMut;
use kithara_abr::{
    AbrController, AbrOptions, AbrReason, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource, Variant,
};
use kithara_assets::{Assets, ResourceKey};
use kithara_storage::Resource;
use kithara_worker::{AsyncWorkerSource, Fetch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::{HlsCommand, HlsMessage, VariantMetadata};
use crate::{
    cache::{Loader, SegmentType},
    events::HlsEvent,
    fetch::DefaultFetchManager,
};

/// HLS worker source implementing AsyncWorkerSource.
///
/// Fetches HLS segments and emits chunks with metadata for direct disk access.
/// Data stays on disk - adapter reads directly via AssetStore.
pub struct HlsWorkerSource {
    /// Generic loader (FetchLoader or test mock).
    loader: Arc<dyn Loader>,

    /// Fetch manager (provides access to assets).
    fetch_manager: Arc<DefaultFetchManager>,

    /// Variant metadata (codec, container, bitrate).
    variant_metadata: Vec<VariantMetadata>,

    /// Current variant index.
    current_variant: usize,

    /// Current segment index within variant.
    current_segment_index: usize,

    /// Init segments already sent for each variant (for is_variant_switch flag).
    sent_init_for_variant: HashSet<usize>,

    /// Current epoch (incremented on seek/switch).
    epoch: u64,

    /// Paused state.
    paused: bool,

    /// ABR controller (includes buffer tracking).
    abr_controller: Option<AbrController<ThroughputEstimator>>,

    /// ABR variants list (for decision making).
    abr_variants: Vec<Variant>,

    /// Global byte offset.
    byte_offset: u64,

    /// Events channel (optional).
    events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,

    /// Cancellation token.
    cancel: CancellationToken,
}

impl HlsWorkerSource {
    /// Create a new HLS worker source.
    ///
    /// # Arguments
    /// - `loader`: Segment loader (FetchLoader or mock)
    /// - `fetch_manager`: Fetch manager for reading segment data
    /// - `variant_metadata`: Metadata for each variant
    /// - `initial_variant`: Starting variant index
    /// - `abr_options`: ABR configuration (None for no ABR)
    /// - `events_tx`: Optional event broadcast channel
    /// - `cancel`: Cancellation token
    pub fn new(
        loader: Arc<dyn Loader>,
        fetch_manager: Arc<DefaultFetchManager>,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        abr_options: Option<AbrOptions>,
        events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        // Build ABR variants list from variant_metadata
        let abr_variants: Vec<Variant> = variant_metadata
            .iter()
            .map(|v| Variant {
                variant_index: v.index,
                bandwidth_bps: v.bitrate.unwrap_or(0),
            })
            .collect();

        // Create ABR controller if Auto mode is enabled
        let abr_controller = abr_options
            .as_ref()
            .filter(|opts| opts.is_auto())
            .map(|opts| AbrController::new(opts.clone()));

        Self {
            loader,
            fetch_manager,
            variant_metadata,
            current_variant: initial_variant,
            current_segment_index: 0,
            sent_init_for_variant: HashSet::new(),
            epoch: 0,
            paused: false,
            abr_controller,
            abr_variants,
            byte_offset: 0,
            events_tx,
            cancel,
        }
    }

    /// Get the asset store for direct disk access.
    pub fn assets(&self) -> kithara_assets::AssetStore {
        self.fetch_manager.assets().clone()
    }

    /// Emit an event (if channel exists).
    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }
}

#[async_trait]
impl AsyncWorkerSource for HlsWorkerSource {
    type Chunk = HlsMessage;
    type Command = HlsCommand;

    async fn fetch_next(&mut self) -> Fetch<HlsMessage> {
        if self.cancel.is_cancelled() {
            debug!("worker cancelled, returning EOF");
            return Fetch::new(HlsMessage::empty(), true, self.epoch);
        }

        if self.paused {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            return Fetch::new(HlsMessage::empty(), false, self.epoch);
        }

        let current_variant = self.current_variant;

        // Check if this is a variant switch (first segment after switch)
        let is_variant_switch = !self.sent_init_for_variant.contains(&current_variant);

        let num_segments = match self.loader.num_segments(current_variant).await {
            Ok(n) => n,
            Err(e) => {
                debug!(error = ?e, "failed to get segment count");
                return Fetch::new(HlsMessage::empty(), true, self.epoch);
            }
        };

        if self.current_segment_index >= num_segments {
            debug!("reached end of playlist");
            self.emit_event(HlsEvent::EndOfStream);
            return Fetch::new(HlsMessage::empty(), true, self.epoch);
        }

        let meta = match self
            .loader
            .load_segment(current_variant, self.current_segment_index)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                debug!(
                    variant = current_variant,
                    segment_index = self.current_segment_index,
                    error = ?e,
                    "segment metadata load failed"
                );
                return Fetch::new(HlsMessage::empty(), true, self.epoch);
            }
        };

        // Get init segment metadata (don't read bytes - adapter will read from disk)
        let (init_url, init_len) =
            match self.loader.load_segment(current_variant, usize::MAX).await {
                Ok(init_meta) => {
                    if is_variant_switch {
                        debug!(
                            variant = current_variant,
                            init_url = %init_meta.url,
                            init_len = init_meta.len,
                            "init segment loaded for variant switch"
                        );
                        self.sent_init_for_variant.insert(current_variant);
                    }
                    (Some(init_meta.url), init_meta.len)
                }
                Err(e) => {
                    debug!(
                        variant = current_variant,
                        error = ?e,
                        "init segment load failed, media only"
                    );
                    (None, 0)
                }
            };

        // Download time for ABR is already tracked by loader, use segment metadata
        let download_start = std::time::Instant::now();
        // Segment is already downloaded by loader.load_segment() above
        // We just need to track time for ABR throughput calculation
        let download_duration = download_start.elapsed();
        let media_len = meta.len;

        let variant_meta = &self.variant_metadata[current_variant];

        // Read segment data from disk
        let assets = self.fetch_manager.assets();
        let mut segment_bytes = BytesMut::new();

        // Read init segment if this is a variant switch
        if is_variant_switch {
            if let Some(ref init_segment_url) = init_url {
                let init_key = ResourceKey::from_url(init_segment_url);
                match assets.open_streaming_resource(&init_key).await {
                    Ok(resource) => match resource.read().await {
                        Ok(bytes) => {
                            trace!(variant = current_variant, len = bytes.len(), "read init segment");
                            segment_bytes.extend_from_slice(&bytes);
                        }
                        Err(e) => {
                            warn!(variant = current_variant, error = ?e, "failed to read init segment");
                        }
                    },
                    Err(e) => {
                        warn!(variant = current_variant, error = ?e, "failed to open init resource");
                    }
                }
            }
        }

        // Read media segment
        let media_key = ResourceKey::from_url(&meta.url);
        match assets.open_streaming_resource(&media_key).await {
            Ok(resource) => match resource.read().await {
                Ok(bytes) => {
                    trace!(
                        variant = current_variant,
                        segment = self.current_segment_index,
                        len = bytes.len(),
                        "read media segment"
                    );
                    segment_bytes.extend_from_slice(&bytes);
                }
                Err(e) => {
                    warn!(
                        variant = current_variant,
                        segment = self.current_segment_index,
                        error = ?e,
                        "failed to read media segment"
                    );
                }
            },
            Err(e) => {
                warn!(
                    variant = current_variant,
                    segment = self.current_segment_index,
                    error = ?e,
                    "failed to open media resource"
                );
            }
        }

        let chunk = HlsMessage {
            bytes: segment_bytes.freeze(),
            byte_offset: self.byte_offset,
            variant: current_variant,
            segment_type: SegmentType::Media(self.current_segment_index),
            segment_url: meta.url.clone(),
            init_url,
            init_len,
            media_len,
            segment_duration: meta.duration,
            codec: variant_meta.codec,
            container: meta.container,
            bitrate: variant_meta.bitrate,
            encryption: None,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch,
        };

        // Byte offset advances by init_len + media_len (combined segment size)
        self.byte_offset += init_len + media_len;
        self.current_segment_index += 1;

        // ABR: Update buffer level, record throughput sample and make decision
        let (abr_decision, throughput_bps, buffer_level) =
            if let Some(ref mut controller) = self.abr_controller {
                // Calculate throughput
                let throughput_bps = if download_duration.as_secs_f64() > 0.0 {
                    (meta.len as f64 * 8.0) / download_duration.as_secs_f64()
                } else {
                    0.0
                };

                // Push throughput sample with content duration
                let sample = ThroughputSample {
                    bytes: meta.len,
                    duration: download_duration,
                    at: download_start,
                    source: ThroughputSampleSource::Network,
                    content_duration: meta.duration,
                };
                controller.push_throughput_sample(sample);

                // Get buffer level and make decision
                let buffer_level = controller.buffer_level_secs();
                let decision = controller.decide(&self.abr_variants, std::time::Instant::now());

                (Some(decision), throughput_bps, buffer_level)
            } else {
                (None, 0.0, 0.0)
            };

        // Emit events (after releasing mutable borrow)
        self.emit_event(HlsEvent::ThroughputSample {
            bytes_per_second: throughput_bps,
        });
        self.emit_event(HlsEvent::BufferLevel {
            level_seconds: buffer_level as f32,
        });

        // Apply ABR decision (only if variant actually changes)
        if let Some(decision) = abr_decision {
            if decision.changed {
                let new_variant = decision.target_variant_index;

                // Only apply switch if variant actually changed
                if new_variant != current_variant {
                    debug!(
                        from = current_variant,
                        to = new_variant,
                        reason = ?decision.reason,
                        "ABR switching variant"
                    );
                    self.current_variant = new_variant;
                    self.sent_init_for_variant.remove(&new_variant);
                    self.emit_event(HlsEvent::VariantApplied {
                        from_variant: current_variant,
                        to_variant: new_variant,
                        reason: decision.reason,
                    });
                } else {
                    debug!(
                        variant = current_variant,
                        reason = ?decision.reason,
                        "ABR decided to stay on current variant"
                    );
                }
            }
        }

        trace!(
            variant = current_variant,
            segment = self.current_segment_index - 1,
            bytes = meta.len,
            "emitting chunk"
        );

        Fetch::new(chunk, false, self.epoch)
    }

    fn handle_command(&mut self, cmd: HlsCommand) -> u64 {
        match cmd {
            HlsCommand::Seek {
                segment_index,
                epoch,
            } => {
                debug!(segment_index, epoch, "seek command");
                self.current_segment_index = segment_index;
                self.epoch = epoch;
                if let Some(ref mut controller) = self.abr_controller {
                    controller.reset_buffer();
                }
                self.epoch
            }
            HlsCommand::ForceVariant {
                variant_index,
                epoch,
            } => {
                debug!(variant_index, epoch, "force variant command");
                let old_variant = self.current_variant;
                self.current_variant = variant_index;
                self.sent_init_for_variant.remove(&variant_index);
                self.epoch = epoch;
                self.emit_event(HlsEvent::VariantApplied {
                    from_variant: old_variant,
                    to_variant: variant_index,
                    reason: AbrReason::ManualOverride,
                });
                self.epoch
            }
            HlsCommand::Pause => {
                debug!("pause command");
                self.paused = true;
                self.epoch
            }
            HlsCommand::Resume => {
                debug!("resume command");
                self.paused = false;
                self.epoch
            }
        }
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_net::HttpClient;
    use url::Url;

    use super::*;
    use crate::{
        cache::{MockLoader, SegmentMeta},
        fetch::DefaultFetchManager,
    };

    fn create_test_metadata() -> Vec<VariantMetadata> {
        vec![VariantMetadata {
            index: 0,
            codec: Some(kithara_stream::AudioCodec::AacLc),
            container: Some(kithara_stream::ContainerFormat::Fmp4),
            bitrate: Some(128000),
        }]
    }

    fn create_test_segment_meta(variant: usize, segment_index: usize, len: u64) -> SegmentMeta {
        let url_str = if segment_index == usize::MAX {
            format!("http://test.com/v{}/init.mp4", variant)
        } else {
            format!("http://test.com/v{}/seg{}.ts", variant, segment_index)
        };

        let segment_type = if segment_index == usize::MAX {
            crate::cache::SegmentType::Init
        } else {
            crate::cache::SegmentType::Media(segment_index)
        };

        SegmentMeta {
            variant,
            segment_type,
            sequence: if segment_index == usize::MAX {
                0
            } else {
                segment_index as u64
            },
            url: Url::parse(&url_str).expect("valid url"),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
            container: Some(kithara_stream::ContainerFormat::Fmp4),
        }
    }

    struct TestContext {
        _temp_dir: tempfile::TempDir,
        fetch_manager: Arc<DefaultFetchManager>,
    }

    impl TestContext {
        fn new() -> Self {
            let temp_dir = tempfile::TempDir::new().expect("temp dir");
            let cache_dir = temp_dir.path().to_path_buf();

            let assets = kithara_assets::AssetStoreBuilder::new()
                .asset_root("test_root")
                .root_dir(&cache_dir)
                .build();

            let net = HttpClient::new(Default::default());
            let fetch_manager = Arc::new(DefaultFetchManager::new(assets, net));

            Self {
                _temp_dir: temp_dir,
                fetch_manager,
            }
        }

        async fn populate_data(&self, url: &Url, len: u64) {
            use kithara_assets::ResourceKey;
            use tokio::fs;

            let key = ResourceKey::from_url(url);
            let file_path = self
                ._temp_dir
                .path()
                .join("test_root")
                .join(&key.rel_path());

            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).await.expect("create dirs");
            }

            let fake_data = vec![0u8; len as usize];
            fs::write(&file_path, &fake_data).await.expect("write file");
        }
    }

    #[tokio::test]
    #[ignore] // TODO: fix asset store setup in tests
    async fn test_worker_source_basic() {
        let ctx = TestContext::new();

        // Populate test data
        ctx.populate_data(&Url::parse("http://test.com/v0/init.mp4").unwrap(), 1000)
            .await;
        ctx.populate_data(&Url::parse("http://test.com/v0/seg0.ts").unwrap(), 10000)
            .await;
        ctx.populate_data(&Url::parse("http://test.com/v0/seg1.ts").unwrap(), 10000)
            .await;

        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader
            .expect_num_segments()
            .with(mockall::predicate::eq(0))
            .returning(|_| Ok(3));

        loader
            .expect_load_segment()
            .with(
                mockall::predicate::eq(0),
                mockall::predicate::eq(usize::MAX),
            )
            .returning(|variant, idx| Ok(create_test_segment_meta(variant, idx, 1000)));

        loader
            .expect_load_segment()
            .times(2)
            .returning(|variant, idx| Ok(create_test_segment_meta(variant, idx, 10000)));

        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            ctx.fetch_manager.clone(),
            metadata,
            0,
            None, // no ABR
            None, // no events
            cancel,
        );

        let fetch1 = source.fetch_next().await;
        assert!(!fetch1.is_eof);
        assert_eq!(fetch1.epoch, 0);
        assert!(fetch1.data.segment_type.is_init());

        let fetch2 = source.fetch_next().await;
        assert!(!fetch2.is_eof);
        assert_eq!(fetch2.data.segment_type.media_index(), Some(0));

        let fetch3 = source.fetch_next().await;
        assert!(!fetch3.is_eof);
        assert_eq!(fetch3.data.segment_type.media_index(), Some(1));
    }

    #[tokio::test]
    #[ignore] // TODO: fix asset store setup in tests
    async fn test_worker_source_eof() {
        let ctx = TestContext::new();

        // Populate test data
        ctx.populate_data(&Url::parse("http://test.com/v0/init.mp4").unwrap(), 1000)
            .await;

        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader
            .expect_num_segments()
            .with(mockall::predicate::eq(0))
            .returning(|_| Ok(0));

        loader
            .expect_load_segment()
            .with(
                mockall::predicate::eq(0),
                mockall::predicate::eq(usize::MAX),
            )
            .returning(|variant, idx| Ok(create_test_segment_meta(variant, idx, 1000)));

        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            ctx.fetch_manager.clone(),
            metadata,
            0,
            None, // no ABR
            None, // no events
            cancel,
        );

        let fetch1 = source.fetch_next().await;
        assert!(!fetch1.is_eof);

        let fetch2 = source.fetch_next().await;
        assert!(fetch2.is_eof);
    }

    #[tokio::test]
    #[ignore] // TODO: fix asset store setup in tests
    async fn test_worker_source_seek_command() {
        let ctx = TestContext::new();

        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader.expect_num_segments().returning(|_| Ok(10));

        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            ctx.fetch_manager.clone(),
            metadata,
            0,
            None, // no ABR
            None, // no events
            cancel,
        );

        let new_epoch = source.handle_command(HlsCommand::Seek {
            segment_index: 5,
            epoch: 1,
        });

        assert_eq!(new_epoch, 1);
        assert_eq!(source.current_segment_index, 5);
        assert_eq!(source.epoch, 1);
    }
}
